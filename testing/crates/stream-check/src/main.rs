//! The frame stream check, against the module's test stream source (`test_stream_source`): frames
//! round trip over the shared-memory ring with the test source, and every way a consumer can lie
//! about the ring or an address is refused with an errno, faulting only its own stream, with nothing
//! left held. The same for a stream whose buffers are dma-bufs of their own
//! (`LEMNOS_STREAM_CREATE_BUFFERS`): each frame lands in its buffer's own memory, a buffer without
//! memory is never filled, memory swaps only while the block can't be writing it, and every pin ends
//! with the stream.
//!
//! Usage: `lemnos-stream-check /dev/lemnos <device name> <unbind path> <heap> [no-source]` (init runs
//! it as `/lemnos_stream_check`). Port of `kernel/test/lemnos_stream_check.c` (CZ-GUEST-4), in the
//! shared-memory ring form of `kernel/test/shm_ring.h` (I2b2-6): a frame is read through the index and
//! checked against its stamp, and a buffer is given back by the consumer's marks page, not by a ring
//! word.
//!
//! The uAPI structures are laid out in bytes, sized as the C structures are: `lemnos.h`'s
//! `lemnos_stream_create` (56 bytes), `lemnos_stream_register` (24), `lemnos_stream_unregister` (8),
//! `lemnos_stream_status` (40), `lemnos_stream_test_source` (8), `lemnos_stream_buffer` (24),
//! `lemnos_stream_reader` (48) and `lemnos_stream_job` (80). The requests that return a descriptor
//! (the stream, the reader's registration) and the control node's allocation go through
//! `lemnos_linux_sys::guest` and `module`, which own the descriptor.
//!
//! Departures from the C, each on purpose and on failure paths only: where the C would go on with a
//! stream, pool, heap or mapping that never opened, the Rust prints the check's FAIL line and ends
//! the run with the summary; a test whose stream never opened gives up its own remaining lines. The
//! doorbell's descriptor is closed after the stream, not before it (the stream holds no reference to
//! it, so nothing observable changes).

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::poll::{POLLHUP, POLLIN, poll_one};
use lemnos_linux_sys::privilege::{self, ChildEnd};
use lemnos_linux_sys::{descriptor, dma_buf, dma_heap, eventfd, guest, module, time};
use lemnos_linux_sys::{ioc, ior, iow, iowr};

// lemnos.h: the stream's requests, on the stream's file.
const KICK: u32 = ioc(0, b'L', 0x81, 0);
const REGISTER: u32 = iowr::<[u8; 24]>(b'L', 0x82);
const UNREGISTER: u32 = iow::<[u8; 8]>(b'L', 0x83);
const STATUS: u32 = ior::<[u8; 40]>(b'L', 0x84);
const TEST_SOURCE: u32 = iow::<[u8; 8]>(b'L', 0x85);
const BUFFER: u32 = iowr::<[u8; 24]>(b'L', 0x86);

// lemnos.h: the ring's end word (LEMNOS_RING_STATE_*).
const STATE_OPEN: u64 = 0;
const STATE_CLOSED: u64 = 1;
const STATE_FAULT: u64 = 2;

// lemnos.h: LEMNOS_STREAM_CREATE_BUFFERS, and the create limits the refusals test.
const CREATE_BUFFERS: u32 = 1 << 0;
const MAX_BUFFERS: u32 = 4096;
const MAX_BUFFER_BYTES: u32 = 1 << 28;
const MAX_JOB_BYTES: u32 = 4096;
const MAX_OWN_BUFFERS: u32 = 64;
/// `LEMNOS_STREAM_JOB_RANGES`.
const JOB_RANGES: usize = 4;

// kernel/test/shm_ring.h (layout 2): the word numbers of the frame ring and the job block. The
// frame index holds, for each frame slot, the lane slot the frame was published in (from bit 16).
// A lane slot is SHM_SLOT_WORDS words: the stamp, the time, the meta, then the 48-byte entry from
// SHM_ENTRY on (the buffer in the pair word, the identity at SHM_ENTRY + 3).
const SHM_W_HEAD: usize = 8;
const SHM_W_END: usize = 10;
const SHM_LANE_HEAD: usize = 16;
const SHM_INDEX: usize = 17;
const SHM_SLOT_WORDS: usize = 10;
const SHM_ENTRY: usize = 4;
const SHM_TICKETS: usize = 8;
const SHM_READERS: usize = 64;
const SHM_JOB_HEAD: usize = 2;
const SHM_JOB_TAIL: usize = 3;
const SHM_JOB_SLOTS: usize = 4;
/// Word 0 of the frame ring: "SHMRING1", little-endian.
const RING_MAGIC: u64 = 0x3147_4e49_524d_4853;
/// Word 0 of the pool page: "SHMPOOL1", little-endian.
const POOL_MAGIC: u64 = 0x314c_4f4f_504d_4853;

const BUFFERS: u32 = 4;
const BUFFER_BYTES: u32 = 8192;
const STRIDE: u64 = 8192;
const POOL_BYTES: u64 = BUFFERS as u64 * STRIDE;
const FRAME_SLOTS: u32 = 8;
const JOB_SLOTS: u32 = 4;
/// `sizeof(struct lemnos_stream_job)`.
const JOB_BYTES: usize = 80;
/// A job slot: its length word, then the job's words.
const JOB_WORDS: usize = 1 + JOB_BYTES / 8;
const AUX_BYTES: u64 = 16384;
const JOB_ROUNDS: u32 = 1500;
/// The byte the test source writes into a range it was given (`0xa5`).
const PATTERN: u8 = 0xa5;

const EACCES: i32 = libc::EACCES;
const EBADF: i32 = libc::EBADF;
const EBUSY: i32 = libc::EBUSY;
const EFAULT: i32 = libc::EFAULT;
const EINVAL: i32 = libc::EINVAL;
const EMSGSIZE: i32 = libc::EMSGSIZE;
const ENODEV: i32 = libc::ENODEV;
const ENOENT: i32 = libc::ENOENT;
const ENOSPC: i32 = libc::ENOSPC;
const EOPNOTSUPP: i32 = libc::EOPNOTSUPP;
const EOVERFLOW: i32 = libc::EOVERFLOW;
const EPERM: i32 = libc::EPERM;
const ERANGE: i32 = libc::ERANGE;
const SIGKILL: i32 = libc::SIGKILL;

/// The checks that failed so far.
static FAILURES: AtomicU32 = AtomicU32::new(0);

fn failures() -> u32 {
    FAILURES.load(Ordering::Relaxed)
}

/// Prints one line, unbuffered as the C check's `printf` is.
fn say(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// One `CHECK`: `ok   ` or `FAIL ` and the message, counted when it failed.
fn line(ok: bool, message: &str) {
    if !ok {
        FAILURES.fetch_add(1, Ordering::Relaxed);
    }
    say(&format!("{}{message}", if ok { "ok   " } else { "FAIL " }));
}

/// `CHECK(cond, fmt, ...)`: the condition first, then the message.
macro_rules! check {
    ($cond:expr, $($arg:tt)+) => {{
        let ok_ = $cond;
        line(ok_, &format!($($arg)+));
    }};
}

/// The summary line the C check ends with, and the exit status it gives.
fn done(label: &str) -> ExitCode {
    say(&format!("{label}: {} failed", failures()));
    if failures() > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
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

/// The errno a mapping was refused with, 0 when it was granted.
fn probe_errno(r: &io::Result<()>) -> i32 {
    r.as_ref().err().map_or(0, errno_of)
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

/// `CLOCK_MONOTONIC` now, in nanoseconds: the clock frame timestamps are on.
fn now_ns() -> u64 {
    time::monotonic_ns().unwrap_or(0)
}

fn sleep_ms(ms: u64) {
    thread::sleep(Duration::from_millis(ms));
}

/// Runs this process on one CPU; the test source's work runs on the CPU that started it.
fn pin(cpu: usize) {
    let _ = guest::set_cpu_affinity(Some(cpu));
}

/// Ends a forked child, if there is one (`child` is -1 for a fork that failed).
fn reap(child: i32) {
    if child > 0 {
        let _ = guest::kill(child, SIGKILL);
        let _ = privilege::wait_child(child);
    }
}

/// The word `at` of a mapping of 8-byte words, read at its byte offset; 0 outside the mapping.
fn map_u64(m: &Mapping, byte_at: usize) -> u64 {
    m.read_volatile::<8>(byte_at).map_or(0, u64::from_ne_bytes)
}

/// The byte at `at` of a mapping; 0 outside it.
fn map_u8(m: &Mapping, at: usize) -> u8 {
    m.as_slice().get(at).copied().unwrap_or(0)
}

/// `memset(m, byte, len)` on a writable mapping.
fn fill(m: &Mapping, byte: u8, len: usize) {
    let _ = m.write_bytes(0, &vec![byte; len]);
}

// ---------------------------------------------------------------------------------------------
// The stream: its layout, its ring, its marks, and the requests on it.
// ---------------------------------------------------------------------------------------------

/// `struct lemnos_stream_create`, the request's fields in.
#[derive(Clone, Copy)]
struct Layout {
    block: u32,
    flags: u32,
    buffers: u32,
    buffer_bytes: u32,
    frame_slots: u32,
    job_slots: u32,
    job_bytes: u32,
    pool_fd: i32,
    doorbell_fd: i32,
}

impl Layout {
    /// `good_layout`: a stream with a pool and the shared-memory ring's sizes.
    fn good(block: u32, pool_fd: i32, doorbell_fd: i32) -> Self {
        Self {
            block,
            flags: 0,
            buffers: BUFFERS,
            buffer_bytes: BUFFER_BYTES,
            frame_slots: FRAME_SLOTS,
            job_slots: JOB_SLOTS,
            job_bytes: JOB_BYTES as u32,
            pool_fd,
            doorbell_fd,
        }
    }

    /// A stream whose buffers are dma-bufs of their own: no pool.
    fn own(block: u32) -> Self {
        let mut l = Self::good(block, -1, -1);
        l.flags = CREATE_BUFFERS;
        l
    }

    fn encode(&self) -> [u8; 56] {
        let mut c = [0u8; 56];
        put32(&mut c, 0, self.block);
        put32(&mut c, 4, self.flags);
        put32(&mut c, 8, self.buffers);
        put32(&mut c, 12, self.buffer_bytes);
        put32(&mut c, 16, self.frame_slots);
        put32(&mut c, 20, self.job_slots);
        put32(&mut c, 24, self.job_bytes);
        put_i32(&mut c, 28, self.pool_fd);
        put_i32(&mut c, 32, self.doorbell_fd);
        c
    }
}

/// `shm_page_at`: the pool page's first word, after the frame ring.
fn shm_page_at(frame_slots: usize) -> usize {
    SHM_INDEX + frame_slots + frame_slots * SHM_SLOT_WORDS
}

/// `shm_control`: the control block's first word, after the pool page's tickets and cut-off words.
fn shm_control(frame_slots: usize, buffers: usize) -> usize {
    shm_page_at(frame_slots) + SHM_TICKETS + buffers + SHM_READERS
}

/// A consumer's marks page (`struct shm_marks`): the page, its registration, and the memfd.
#[derive(Default)]
struct Marks {
    page: Option<Mapping>,
    _reader: Option<OwnedFd>,
    _fd: Option<OwnedFd>,
    buffers: u32,
    version: u64,
}

/// A stream the check holds: its ring, its marks, and what the consumer knows of it.
struct Stream<'a> {
    /// The ring. Dropped first, as the C closes the ring before the marks and the descriptor.
    ring: Mapping,
    marks: Marks,
    fd: OwnedFd,
    /// The consumer's doorbell, which the caller keeps.
    bell: Option<BorrowedFd<'a>>,
    ring_bytes: usize,
    pool_addr: u64,
    control: usize,
    jobs_at: usize,
    jobs_head: u64,
    frames_tail: u64,
    /// Frames whose buffer was taken back before the mark landed: never read, counted.
    dropped: u64,
    /// Doorbell counts drained as they are read, so the total is the sum of the drains.
    drained: u64,
}

impl Stream<'_> {
    /// The ring's words, as atomics. Empty only for a mapping that is not whole words.
    fn words(&self) -> &[AtomicU64] {
        self.ring.as_atomic_words().unwrap_or(&[])
    }

    /// A ring word, acquire. A word past the ring reads as 0.
    fn load(&self, at: usize) -> u64 {
        self.words()
            .get(at)
            .map_or(0, |w| w.load(Ordering::Acquire))
    }

    /// A ring word, release. A word past the ring is not written.
    fn store(&self, at: usize, v: u64) {
        if let Some(w) = self.words().get(at) {
            w.store(v, Ordering::Release);
        }
    }
}

/// Makes a stream as the create request says, with `bell` as its doorbell.
fn make<'a>(
    ctl: BorrowedFd<'_>,
    l: &Layout,
    bell: Option<BorrowedFd<'a>>,
) -> Result<Stream<'a>, i32> {
    let mut c = l.encode();
    let fd = guest::stream_create(ctl, &mut c).map_err(|e| errno_of(&e))?;
    let ring_bytes = get64(&c, 40) as usize;
    let ring = Mapping::new(fd.as_fd(), ring_bytes, 0, true).map_err(|e| errno_of(&e))?;
    let control = shm_control(FRAME_SLOTS as usize, BUFFERS as usize);
    Ok(Stream {
        ring,
        marks: Marks::default(),
        fd,
        bell,
        ring_bytes,
        pool_addr: get64(&c, 48),
        control,
        jobs_at: control + SHM_JOB_SLOTS,
        jobs_head: 0,
        frames_tail: 0,
        dropped: 0,
        drained: 0,
    })
}

/// A stream on `pool`, with no doorbell.
fn open_with(ctl: BorrowedFd<'_>, block: u32, pool: i32) -> Result<Stream<'static>, i32> {
    make(ctl, &Layout::good(block, pool, -1), None)
}

/// A stream on `pool` with `bell` as its doorbell.
fn open_bell<'a>(
    ctl: BorrowedFd<'_>,
    block: u32,
    pool: i32,
    bell: BorrowedFd<'a>,
) -> Result<Stream<'a>, i32> {
    make(
        ctl,
        &Layout::good(block, pool, bell.as_raw_fd()),
        Some(bell),
    )
}

/// The errno a create was refused with, 0 when the module made the stream (which is then closed).
fn create_err(ctl: BorrowedFd<'_>, l: &Layout) -> i32 {
    match guest::stream_create(ctl, &mut l.encode()) {
        Ok(_) => 0,
        Err(e) => errno_of(&e),
    }
}

/// The shared test environment: the control node, the block and its name, the pool and the aux
/// dma-buf, and the heap the dma-bufs come from.
#[derive(Clone, Copy)]
struct Env<'a> {
    ctl: BorrowedFd<'a>,
    block: u32,
    name: &'a str,
    pool: i32,
    aux: i32,
    heap: BorrowedFd<'a>,
}

impl Env<'_> {
    /// `CHECK(!stream_open(&s, ctl, block, pool, -1), what)`: the stream, when it opened.
    fn open(&self, what: &str) -> Option<Stream<'static>> {
        let r = open_with(self.ctl, self.block, self.pool);
        check!(r.is_ok(), "{what}");
        r.ok()
    }
}

/// A job (`struct lemnos_stream_job`), in its slot.
#[derive(Clone, Copy, Default)]
struct Range {
    addr: u64,
    len: u64,
}

#[derive(Clone, Copy, Default)]
struct Job {
    identity: u64,
    buffer: u32,
    count: u32,
    range: [Range; JOB_RANGES],
}

impl Job {
    /// `job(buffer, identity, count)`: no ranges set.
    fn new(buffer: u32, identity: u64, count: u32) -> Self {
        Self {
            identity,
            buffer,
            count,
            range: [Range::default(); JOB_RANGES],
        }
    }

    /// The job with range `i` set to `addr` and `len`.
    fn with(mut self, i: usize, addr: u64, len: u64) -> Self {
        self.range[i] = Range { addr, len };
        self
    }

    fn bytes(&self) -> [u8; JOB_BYTES] {
        let mut b = [0u8; JOB_BYTES];
        put64(&mut b, 0, self.identity);
        put32(&mut b, 8, self.buffer);
        put32(&mut b, 12, self.count);
        for (i, r) in self.range.iter().enumerate() {
            put64(&mut b, 16 + 16 * i, r.addr);
            put64(&mut b, 24 + 16 * i, r.len);
        }
        b
    }
}

/// A frame as the consumer reads it: its buffer, its bytes, its fence, its sequence and identity.
#[derive(Default, Clone, Copy)]
struct Frame {
    buffer: u32,
    bytes: u32,
    fence: u32,
    sequence: u64,
    identity: u64,
    timestamp_ns: u64,
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
        buffers,
        version: 0,
    };
    true
}

/// The page's words, as atomics; empty for a mapping that is not whole words.
fn words(m: &Mapping) -> &[AtomicU64] {
    m.as_atomic_words().unwrap_or(&[])
}

/// `shm_marks_set`: writes one mark, with the version odd while it changes.
fn marks_set(m: &mut Marks, buffer: u32, mark: u64) {
    let Some(page) = &m.page else {
        return;
    };
    let w = words(page);
    let mask = (1u64 << 56) - 1;
    m.version = (m.version + 1) & mask;
    if let Some(v) = w.first() {
        v.store(2 << 56 | m.version, Ordering::SeqCst);
    }
    if let Some(b) = w.get(1 + buffer as usize) {
        b.store(mark, Ordering::SeqCst);
    }
    m.version = (m.version + 1) & mask;
    if let Some(v) = w.first() {
        v.store(2 << 56 | m.version, Ordering::SeqCst);
    }
}

/// `shm_marks_taken`: the consumer read the frame with ring sequence `seq` from `buffer`.
fn marks_taken(m: &Marks, buffer: u32, seq: u64) {
    if let Some(page) = &m.page
        && let Some(b) = words(page).get(1 + m.buffers as usize + buffer as usize)
    {
        b.store(seq + 1, Ordering::SeqCst);
    }
}

/// `hold_buffer`: the consumer holds `buffer` by its marks. 0, or -1 when the marks page can't be
/// made.
fn hold_buffer(s: &mut Stream<'_>, buffer: u32) -> i32 {
    if s.marks.page.is_none() && !marks_open(&mut s.marks, s.fd.as_fd(), BUFFERS) {
        return -1;
    }
    marks_set(&mut s.marks, buffer, 1);
    0
}

/// `give_back`: the consumer is done with a frame's buffer, so a job may name it.
fn give_back(s: &mut Stream<'_>, buffer: u32) {
    if s.marks.page.is_some() {
        marks_set(&mut s.marks, buffer, 0);
    }
}

/// `pop_frame`: the next frame, if one is published, read through the index and checked against its
/// stamp, so a slot the module has reused is never taken for the frame asked for.
fn pop_frame(s: &mut Stream<'_>, f: &mut Frame) -> bool {
    let head = s.load(SHM_W_HEAD);
    if head == s.frames_tail {
        return false;
    }
    let seq = s.frames_tail;
    let index = SHM_INDEX + (seq & u64::from(FRAME_SLOTS - 1)) as usize;
    let slot = (s.load(index) >> 16) as usize;
    let at = SHM_INDEX + FRAME_SLOTS as usize + slot.wrapping_mul(SHM_SLOT_WORDS);
    if s.load(at) != seq.wrapping_add(1) << 1 {
        return false;
    }
    let pair = s.load(at + SHM_ENTRY + 1);
    let fence = s.load(at + SHM_ENTRY + 4);
    f.buffer = pair as u32;
    f.sequence = s.load(at + SHM_ENTRY + 2);
    f.identity = s.load(at + SHM_ENTRY + 3);
    f.bytes = fence as u32;
    f.fence = (fence >> 32) as u32;
    f.timestamp_ns = s.load(at + 1);
    s.frames_tail += 1;
    // The frame is the consumer's until give_back: its buffer keeps the module off it. The mark
    // comes first, then the ticket is checked against the entry's: a buffer the module took back
    // before the mark landed carries another frame now, so this frame is dropped unread.
    if hold_buffer(s, f.buffer) != 0 {
        say(&format!(
            "FAIL the consumer could not mark buffer {}",
            f.buffer
        ));
        return false;
    }
    let ticket = s.load(shm_page_at(FRAME_SLOTS as usize) + SHM_TICKETS + f.buffer as usize);
    if ticket & 0xffff_ffff != pair >> 32 {
        give_back(s, f.buffer);
        s.dropped += 1;
        return false;
    }
    // Read: the frame counts as taken, so the module may take its buffer back once let go.
    marks_taken(&s.marks, f.buffer, seq);
    true
}

/// `wait_frame`: waits for a frame, on the doorbell when the stream has one (drained as it is read).
/// A stream with no doorbell has nothing that wakes its consumer for a frame (the stream's own
/// descriptor is readable only at its end), so it looks again every millisecond: the test source's
/// next frame is at least one kernel tick (1 ms) away. True with `f` filled, or false once
/// `timeout_ms` have gone by.
fn wait_frame(s: &mut Stream<'_>, f: &mut Frame, timeout_ms: u32) -> bool {
    let until = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
    while Instant::now() < until {
        if pop_frame(s, f) {
            return true;
        }
        let ready = {
            let (fd, step) = match s.bell {
                Some(bell) => (bell, 10),
                None => (s.fd.as_fd(), 1),
            };
            poll_one(fd, POLLIN, Some(Duration::from_millis(step))).unwrap_or(0)
        };
        if ready != 0
            && let Some(bell) = s.bell
        {
            s.drained += eventfd::drain(bell);
        }
    }
    pop_frame(s, f)
}

/// `KICK`: 0, or -errno.
fn kick(s: &Stream<'_>) -> i32 {
    match guest::ioctl_bytes(s.fd.as_fd(), KICK, &mut [0u8; 0]) {
        Ok(()) => 0,
        Err(e) => -errno_of(&e),
    }
}

/// `STATUS`, as the request gives it (all 0xff when the request fails).
#[derive(Clone, Copy)]
struct Status {
    state: u64,
    fault: i32,
    frames: u64,
    dropped: u64,
    jobs: u64,
}

fn status(s: &Stream<'_>) -> Status {
    let mut b = [0xffu8; 40];
    let _ = guest::ioctl_bytes(s.fd.as_fd(), STATUS, &mut b);
    Status {
        state: u64::from(get32(&b, 0)),
        fault: get32(&b, 4) as i32,
        frames: get64(&b, 8),
        dropped: get64(&b, 16),
        jobs: get64(&b, 32),
    }
}

/// `LEMNOS_STREAM_IOC_TEST_SOURCE`: 0, or -errno.
fn run_source(s: &Stream<'_>, period_us: u32, frames: u32) -> i32 {
    let mut a = [0u8; 8];
    put32(&mut a, 0, period_us);
    put32(&mut a, 4, frames);
    match guest::ioctl_bytes(s.fd.as_fd(), TEST_SOURCE, &mut a) {
        Ok(()) => 0,
        Err(e) => -errno_of(&e),
    }
}

/// `post_raw`: a job of `len` in the next slot, from the bytes of `job`.
fn post_raw(s: &mut Stream<'_>, len: u64, job: &[u8]) {
    let slot = (s.jobs_head & u64::from(JOB_SLOTS - 1)) as usize;
    let at = s.jobs_at + slot * JOB_WORDS;
    let mut words = [0u8; JOB_BYTES];
    let n = job.len().min(JOB_BYTES);
    words[..n].copy_from_slice(&job[..n]);
    s.store(at, len);
    for i in 0..JOB_BYTES / 8 {
        s.store(at + 1 + i, get64(&words, 8 * i));
    }
    s.jobs_head += 1;
    s.store(s.control + SHM_JOB_HEAD, s.jobs_head);
}

fn post(s: &mut Stream<'_>, j: &Job) {
    post_raw(s, JOB_BYTES as u64, &j.bytes());
}

/// A faulted stream: KICK's errno, STATUS's, and the ring's state word.
fn expect_fault(s: &Stream<'_>, want: i32, what: &str) {
    let k = kick(s);
    let st = status(s);
    let ring = s.load(SHM_W_END);
    check!(
        k == -want && st.state == STATE_FAULT && st.fault == want && ring == STATE_FAULT,
        "{what}: {} (kick {k}, status state {} errno {}, ring state {ring})",
        strerror(want),
        st.state,
        st.fault,
    );
}

/// `LEMNOS_STREAM_IOC_REGISTER`'s registration, as the request gives it.
#[derive(Clone, Copy, Default)]
struct Reg {
    region: u32,
    addr: u64,
    size: u64,
}

/// `LEMNOS_STREAM_IOC_REGISTER` of `fd`: the outcome (an errno), and what the request left.
fn register(s: &Stream<'_>, fd: i32) -> (Result<(), i32>, Reg) {
    let mut a = [0u8; 24];
    put_i32(&mut a, 0, fd);
    let res = guest::ioctl_bytes(s.fd.as_fd(), REGISTER, &mut a).map_err(|e| errno_of(&e));
    (
        res,
        Reg {
            region: get32(&a, 4),
            addr: get64(&a, 8),
            size: get64(&a, 16),
        },
    )
}

/// `LEMNOS_STREAM_IOC_UNREGISTER` of `region`: the outcome, as an errno.
fn unregister(s: &Stream<'_>, region: u32) -> Result<(), i32> {
    let mut u = [0u8; 8];
    put32(&mut u, 0, region);
    guest::ioctl_bytes(s.fd.as_fd(), UNREGISTER, &mut u).map_err(|e| errno_of(&e))
}

/// `stream_with_aux`: a stream on `pool` with `aux` registered. The stream (when it opened), the
/// region (or -errno) and the dma-buf's device address (0 unless it was registered).
fn stream_with_aux(
    ctl: BorrowedFd<'_>,
    block: u32,
    pool: i32,
    aux: i32,
) -> (Option<Stream<'static>>, i32, u64) {
    let s = match open_with(ctl, block, pool) {
        Ok(s) => s,
        Err(e) => return (None, -e, 0),
    };
    match register(&s, aux) {
        (Ok(()), r) => (Some(s), r.region as i32, r.addr),
        (Err(e), _) => (Some(s), -e, 0),
    }
}

// ---------------------------------------------------------------------------------------------
// Buffers of their own: each one's memory, and the module's view of it.
// ---------------------------------------------------------------------------------------------

/// `struct lemnos_stream_buffer`'s out fields: a buffer's device address and size.
#[derive(Default, Clone, Copy)]
struct Buffer {
    addr: u64,
    size: u64,
}

/// `LEMNOS_STREAM_IOC_BUFFER`: gives buffer `buffer` the dma-buf `fd`. 0, or -errno. `out` is
/// written on success.
fn set_buffer(s: &Stream<'_>, buffer: u32, fd: i32, out: Option<&mut Buffer>) -> i32 {
    let mut a = [0u8; 24];
    put32(&mut a, 0, buffer);
    put_i32(&mut a, 4, fd);
    match guest::ioctl_bytes(s.fd.as_fd(), BUFFER, &mut a) {
        Err(e) => -errno_of(&e),
        Ok(()) => {
            if let Some(o) = out {
                *o = Buffer {
                    addr: get64(&a, 8),
                    size: get64(&a, 16),
                };
            }
            0
        }
    }
}

/// One buffer's memory, from the block: its dma-buf, its device address and its CPU mapping.
#[derive(Default)]
struct Own {
    fd: Option<OwnedFd>,
    addr: u64,
    cpu: Option<Mapping>,
}

fn own_alloc(ctl: BorrowedFd<'_>, block: u32, size: u64) -> Own {
    let Ok(mem) = module::alloc(ctl, block, size) else {
        return Own::default();
    };
    let Ok(cpu) = Mapping::new(mem.fd.as_fd(), size as usize, 0, true) else {
        return Own::default();
    };
    Own {
        fd: Some(mem.fd),
        addr: mem.addr,
        cpu: Some(cpu),
    }
}

impl Own {
    fn raw(&self) -> i32 {
        self.fd.as_ref().map_or(-1, AsRawFd::as_raw_fd)
    }

    fn fill(&self, byte: u8, len: usize) {
        if let Some(cpu) = &self.cpu {
            fill(cpu, byte, len);
        }
    }

    fn byte(&self, at: usize) -> u8 {
        self.cpu.as_ref().map_or(0, |m| map_u8(m, at))
    }

    fn word(&self, at: usize) -> u64 {
        self.cpu.as_ref().map_or(0, |m| map_u64(m, at))
    }

    fn free(&mut self) {
        self.cpu = None;
        self.fd = None;
    }
}

/// The number of dma-bufs attached to device `name`, from the kernel's own list (-1 without it).
fn attached(name: &str) -> i32 {
    let Ok(f) = File::open("/sys/kernel/debug/dma_buf/bufinfo") else {
        return -1;
    };
    let mut buf = Vec::new();
    // As the C reads it: up to the buffer's size less one, and whatever was read counts.
    let _ = f.take((1 << 18) - 1).read_to_end(&mut buf);
    let line = format!("\t{name}\n");
    let needle = line.as_bytes();
    let mut count = 0;
    let mut at = 0;
    while let Some(p) = find(&buf[at..], needle) {
        count += 1;
        at += p + needle.len();
    }
    count
}

/// Whether the block is behind an IOMMU: there every attachment of a dma-buf is a mapping of its
/// own, so the address a stream gives a buffer is not the one the dma-buf was made at.
fn behind_iommu(name: &str) -> bool {
    Path::new(&format!("/sys/bus/platform/devices/{name}/iommu_group")).exists()
}

/// The stream's address for a buffer's memory: the dma-buf's own without an IOMMU.
fn same_place(name: &str, stream_addr: u64, made_at: u64) -> bool {
    stream_addr != 0 && (stream_addr == made_at || behind_iommu(name))
}

/// `alloc_pool`: `size` bytes of the block's memory, as a dma-buf.
fn alloc_pool(ctl: BorrowedFd<'_>, block: u32, size: u64) -> Option<OwnedFd> {
    module::alloc(ctl, block, size).ok().map(|m| m.fd)
}

// ---------------------------------------------------------------------------------------------
// The checks, in the C file's order.
// ---------------------------------------------------------------------------------------------

/// A stream's create refused, with the layout edited by `edit` on a pool stream.
fn refuse(e: &Env<'_>, name: &str, edit: impl FnOnce(&mut Layout)) {
    let mut l = Layout::good(e.block, e.pool, -1);
    edit(&mut l);
    check!(create_err(e.ctl, &l) == EINVAL, "{name}: EINVAL");
}

fn test_refusals_at_create(e: &Env<'_>) {
    refuse(e, "unknown flags", |l| l.flags = 1 << 30);
    refuse(e, "a known flag with an unknown one", |l| {
        l.flags = CREATE_BUFFERS | (1 << 30)
    });
    refuse(e, "own buffers and a pool descriptor", |l| {
        l.flags = CREATE_BUFFERS
    });
    // The shared-memory form was a flag until the frame ring became the only form: that bit is now
    // unknown.
    refuse(e, "the removed shm flag", |l| l.flags = 1 << 1);
    refuse(e, "no buffers", |l| l.buffers = 0);
    refuse(e, "too many buffers", |l| l.buffers = MAX_BUFFERS + 1);
    refuse(e, "zero buffer bytes", |l| l.buffer_bytes = 0);
    refuse(e, "too-large buffers", |l| {
        l.buffer_bytes = MAX_BUFFER_BYTES + 1
    });
    refuse(e, "frame slots not a power of two", |l| l.frame_slots = 3);
    refuse(e, "fewer frame slots than buffers", |l| l.frame_slots = 2);
    {
        let mut l = Layout::good(e.block, e.pool, -1);
        l.buffers = 1;
        l.frame_slots = 1;
        check!(
            create_err(e.ctl, &l) == EINVAL,
            "one frame slot: EINVAL (the frame ring needs two)"
        );
    }
    refuse(e, "no job slots", |l| l.job_slots = 0);
    refuse(e, "too many job slots", |l| l.job_slots = 128);
    refuse(e, "job smaller than the job struct", |l| {
        l.job_bytes = JOB_BYTES as u32 - 8
    });
    refuse(e, "too-large jobs", |l| l.job_bytes = MAX_JOB_BYTES + 1);
    {
        let mut l = Layout::good(e.block, e.pool, -1);
        l.buffers = 4096;
        l.buffer_bytes = 1 << 28;
        l.frame_slots = 8192;
        check!(
            create_err(e.ctl, &l) == EINVAL,
            "the whole pool past its limit: EINVAL"
        );
    }

    let l = Layout::good(e.block, -1, -1);
    check!(create_err(e.ctl, &l) == EBADF, "no pool: EBADF");
    let l = Layout::good(e.block, e.pool, 9999);
    check!(
        create_err(e.ctl, &l) == EBADF,
        "a doorbell that isn't an eventfd: EBADF"
    );
    let l = Layout::good(e.block, 0, -1);
    let err = create_err(e.ctl, &l);
    check!(
        err == EINVAL,
        "a pool that isn't a dma-buf: refused ({})",
        strerror(err)
    );

    match module::alloc(e.ctl, e.block, STRIDE) {
        Ok(small) => {
            let l = Layout::good(e.block, small.fd.as_raw_fd(), -1);
            check!(
                create_err(e.ctl, &l) == EINVAL,
                "a pool smaller than the layout: EINVAL"
            );
        }
        Err(_) => check!(false, "small alloc"),
    }

    let Some(s) = e.open("a good stream") else {
        return;
    };
    let ring = s.ring_bytes;
    let r = guest::mapping_probe(
        s.fd.as_fd(),
        ring + 4096,
        0,
        libc::PROT_READ,
        libc::MAP_SHARED,
    );
    check!(
        r.is_err(),
        "a mapping longer than the ring: refused ({})",
        strerror(probe_errno(&r))
    );
    let r = guest::mapping_probe(s.fd.as_fd(), ring, 0, libc::PROT_READ, libc::MAP_PRIVATE);
    check!(
        r.is_err(),
        "a private mapping: refused ({})",
        strerror(probe_errno(&r))
    );
    let r = guest::mapping_probe(
        s.fd.as_fd(),
        ring,
        0,
        libc::PROT_READ | libc::PROT_EXEC,
        libc::MAP_SHARED,
    );
    check!(
        r.is_err(),
        "an executable mapping: refused ({})",
        strerror(probe_errno(&r))
    );
}

fn test_register(e: &Env<'_>) {
    let Some(s) = e.open("open") else {
        return;
    };
    let (r, _) = register(&s, e.pool);
    check!(r == Err(EBUSY), "registering the pool again: EBUSY");
    drop(s);

    let (s, region, _) = stream_with_aux(e.ctl, e.block, e.pool, e.aux);
    check!(region > 0, "registered a heap dma-buf");
    if let Some(s) = &s
        && region > 0
    {
        check!(
            register(s, e.aux).0 == Err(EBUSY),
            "the same dma-buf twice: EBUSY"
        );
        check!(register(s, -1).0 == Err(EBADF), "a bad fd: EBADF");
        check!(
            unregister(s, 999) == Err(EINVAL),
            "a region past the table: EINVAL"
        );
    }
}

/// The table and the number of streams are fixed at the size the module was built with.
fn test_limits(e: &Env<'_>) {
    let mut streams: Vec<Stream<'static>> = Vec::new();
    while streams.len() < 64 {
        match open_with(e.ctl, e.block, e.pool) {
            Ok(s) => streams.push(s),
            Err(_) => break,
        }
    }
    let n = streams.len();
    check!(n == 64, "{n} streams held");
    check!(
        open_with(e.ctl, e.block, e.pool).err() == Some(ENOSPC),
        "one more stream: ENOSPC"
    );
    while let Some(s) = streams.pop() {
        drop(s);
    }

    let Some(s) = e.open("open") else {
        return;
    };
    let mut fds: Vec<OwnedFd> = Vec::new();
    let mut got = 0;
    for _ in 0..16 {
        let mut err = 0;
        let mut registered = false;
        match dma_heap::allocate(e.heap, 4096) {
            Ok(fd) => {
                match register(&s, fd.as_raw_fd()).0 {
                    Ok(()) => {
                        got += 1;
                        registered = true;
                    }
                    Err(x) => err = x,
                }
                fds.push(fd);
            }
            Err(x) => err = errno_of(&x),
        }
        if !registered && err == ENOSPC {
            break;
        }
    }
    check!(got == 15, "{got} dma-bufs registered, then ENOSPC");
    drop(fds);
    drop(s);
}

/// The checks of `LEMNOS_STREAM_CREATE_BUFFERS` and `LEMNOS_STREAM_IOC_BUFFER` that need no frames.
fn test_own_refusals(e: &Env<'_>) {
    let before = attached(e.name);
    let mut l = Layout::own(e.block);
    l.buffers = MAX_OWN_BUFFERS + 1;
    l.frame_slots = 128;
    check!(
        create_err(e.ctl, &l) == EINVAL,
        "own buffers: more than the table holds: EINVAL"
    );

    let Some(s) = e.open("a stream with a pool") else {
        return;
    };
    check!(
        set_buffer(&s, 0, e.aux, None) == -EOPNOTSUPP,
        "giving a buffer memory on a stream with a pool: EOPNOTSUPP"
    );
    drop(s);

    let Ok(s) = make(e.ctl, &Layout::own(e.block), None) else {
        check!(false, "a stream with no pool, pool_addr 0");
        return;
    };
    check!(s.pool_addr == 0, "a stream with no pool, pool_addr 0");
    drop(s);

    let Ok(mut s) = make(e.ctl, &Layout::own(e.block), None) else {
        return;
    };
    check!(attached(e.name) == before, "it pins nothing yet");
    let mut o: Vec<Own> = (0..BUFFERS)
        .map(|i| {
            let size = if i == 3 {
                2 * u64::from(BUFFER_BYTES)
            } else {
                u64::from(BUFFER_BYTES)
            };
            own_alloc(e.ctl, e.block, size)
        })
        .collect();
    let mut spare = own_alloc(e.ctl, e.block, u64::from(BUFFER_BYTES));
    check!(
        o[0].fd.is_some()
            && o[1].fd.is_some()
            && o[2].fd.is_some()
            && o[3].fd.is_some()
            && spare.fd.is_some(),
        "five dma-bufs of the block's"
    );

    check!(
        set_buffer(&s, BUFFERS, o[0].raw(), None) == -EINVAL,
        "a buffer number past the last: EINVAL"
    );
    check!(set_buffer(&s, 0, -1, None) == -EBADF, "a bad fd: EBADF");
    check!(
        set_buffer(&s, 0, e.ctl.as_raw_fd(), None) == -EINVAL,
        "a file that isn't a dma-buf: EINVAL"
    );
    let small = dma_heap::allocate(e.heap, 4096).ok();
    check!(
        small
            .as_ref()
            .is_some_and(|f| set_buffer(&s, 0, f.as_raw_fd(), None) == -EINVAL),
        "a dma-buf smaller than a buffer: EINVAL"
    );
    drop(small);
    check!(
        attached(e.name) == before,
        "a refused dma-buf isn't left pinned"
    );

    // A job on a buffer with no memory, on a stream of its own because it ends it.
    let Ok(mut s2) = make(e.ctl, &Layout::own(e.block), None) else {
        check!(false, "another stream with no pool");
        return;
    };
    check!(true, "another stream with no pool");
    post(&mut s2, &Job::new(0, 1, 0));
    expect_fault(&s2, EFAULT, "a job on a buffer with no memory");
    check!(
        set_buffer(&s2, 0, o[0].raw(), None) == -EFAULT,
        "a faulted stream takes no memory"
    );
    drop(s2);

    let mut out = Buffer::default();
    for i in 0..BUFFERS {
        let ret = set_buffer(&s, i, o[i as usize].raw(), Some(&mut out));
        let made = o[i as usize].addr;
        check!(
            ret == 0 && same_place(e.name, out.addr, made) && out.size == u64::from(BUFFER_BYTES),
            "buffer {i} has its own dma-buf: {ret}, at {:x} (the block's {:x}), {} bytes",
            out.addr,
            made,
            out.size
        );
        o[i as usize].addr = out.addr;
    }
    check!(
        attached(e.name) == before + BUFFERS as i32,
        "one pin for each"
    );
    out = Buffer::default();
    check!(
        set_buffer(&s, 0, o[0].raw(), Some(&mut out)) == 0 && out.addr == o[0].addr,
        "the dma-buf a buffer already has: nothing changes, same address"
    );
    check!(
        attached(e.name) == before + BUFFERS as i32,
        "and no second pin"
    );
    check!(
        set_buffer(&s, 1, o[0].raw(), None) == -EBUSY,
        "another buffer's dma-buf: EBUSY"
    );
    check!(
        register(&s, o[2].raw()).0 == Err(EBUSY),
        "registering a buffer's dma-buf: EBUSY"
    );
    check!(
        register(&s, e.aux).0.is_ok(),
        "a dma-buf registered beside them"
    );
    check!(
        set_buffer(&s, 3, e.aux, None) == -EBUSY,
        "a registered dma-buf as a buffer: EBUSY"
    );

    // Swapping a free buffer's memory: the old pin goes, the new address is the buffer's.
    check!(
        set_buffer(&s, 3, spare.raw(), Some(&mut out)) == 0
            && same_place(e.name, out.addr, spare.addr),
        "a free buffer's memory is swapped"
    );
    check!(
        attached(e.name) == before + BUFFERS as i32 + 1,
        "the old dma-buf's pin went with it"
    );
    let made_at = o[3].addr;
    check!(
        set_buffer(&s, 3, o[3].raw(), Some(&mut out)) == 0
            && same_place(e.name, out.addr, made_at)
            && out.size == u64::from(BUFFER_BYTES),
        "and swapped back: a longer dma-buf reaches only a buffer's bytes"
    );
    o[3].addr = out.addr;

    // A job holds its buffer: its memory can't change under it.
    let j = Job::new(1, 7, 1).with(0, o[1].addr + 512, 16);
    post(&mut s, &j);
    check!(
        kick(&s) == 0 && status(&s).jobs == 1,
        "a job naming a range in its own buffer: accepted"
    );
    check!(
        set_buffer(&s, 1, spare.raw(), None) == -EBUSY,
        "swapping a buffer a job names: EBUSY"
    );
    check!(
        set_buffer(&s, 1, o[1].raw(), None) == 0,
        "naming what it has is still answered"
    );
    let j = Job::new(2, 8, 1).with(0, o[3].addr, 16);
    post(&mut s, &j);
    expect_fault(&s, EACCES, "a range in another buffer's dma-buf");
    drop(s);

    // Past a buffer's bytes but inside its (longer) dma-buf: not the buffer's, so nowhere.
    let Ok(mut s) = make(e.ctl, &Layout::own(e.block), None) else {
        check!(false, "again, one buffer");
        return;
    };
    let ok = set_buffer(&s, 3, o[3].raw(), Some(&mut out)) == 0;
    check!(ok, "again, one buffer");
    o[3].addr = out.addr;
    let j = Job::new(3, 1, 1).with(0, o[3].addr + u64::from(BUFFER_BYTES) - 8, 16);
    post(&mut s, &j);
    expect_fault(&s, EFAULT, "a range running past the buffer's bytes");
    drop(s);

    let left = attached(e.name) - before;
    check!(
        left == 0,
        "closing the streams released every pin ({left} left)"
    );
    for m in &mut o {
        m.free();
    }
    spare.free();
}

/// Frames land in each buffer's own dma-buf, and in the new one after a swap.
fn test_own_frames(e: &Env<'_>) {
    let before = attached(e.name);
    let mut o: Vec<Own> = (0..BUFFERS)
        .map(|_| own_alloc(e.ctl, e.block, u64::from(BUFFER_BYTES)))
        .collect();
    let mut fresh = own_alloc(e.ctl, e.block, u64::from(BUFFER_BYTES));
    let mut other = own_alloc(e.ctl, e.block, u64::from(BUFFER_BYTES));
    let opened = make(e.ctl, &Layout::own(e.block), None);
    check!(
        opened.is_ok() && fresh.fd.is_some() && other.fd.is_some() && o[3].fd.is_some(),
        "own buffers: stream and memory"
    );
    let Ok(mut s) = opened else {
        return;
    };
    let mut f = Frame::default();
    let mut swapped = 0i32;
    let mut busy = 0i32;
    let mut odd = 0i32;
    let mut seen: u32 = 0;
    let bell_word = BUFFER_BYTES as usize - 8;

    check!(
        run_source(&s, 0, 2) == 0,
        "the source runs with no buffer having memory"
    );
    sleep_ms(50);
    check!(
        !pop_frame(&mut s, &mut f) && status(&s).frames == 0,
        "and no frame is made"
    );
    o[2].fill(0x11, BUFFER_BYTES as usize);
    check!(
        set_buffer(&s, 2, o[2].raw(), None) == 0,
        "buffer 2 gets memory"
    );
    check!(
        wait_frame(&mut s, &mut f, 5000)
            && f.buffer == 2
            && o[2].word(0) == f.sequence
            && o[2].word(bell_word) == f.sequence,
        "frame 0 is on the only buffer with memory, in that memory (buffer {}, content {})",
        f.buffer,
        o[2].word(0)
    );
    give_back(&mut s, f.buffer);

    // The consumer has read buffer 2's frame and let it go. That frame counts as taken, so the newest
    // frame's buffer may be taken back (docs/streams.md, the replacement rule), and the source's
    // second frame is made on buffer 2, the only buffer with memory. The consumer keeps that frame,
    // so buffer 2 is held: the source then has no buffer for each frame it makes, and drops and
    // counts it (lem_stream_drop_frame_locked).
    check!(
        wait_frame(&mut s, &mut f, 5000) && f.buffer == 2,
        "the newest, read and let go, is taken back: buffer 2 gets the next frame"
    );
    // The source keeps running and drops each frame it has no buffer for, counted.
    sleep_ms(300);
    check!(
        status(&s).dropped >= 1,
        "frames with no free buffer are dropped and counted ({})",
        status(&s).dropped
    );
    give_back(&mut s, f.buffer);

    for i in 0..BUFFERS {
        o[i as usize].fill(0x11, BUFFER_BYTES as usize);
        if i != 2 {
            check!(
                set_buffer(&s, i, o[i as usize].raw(), None) == 0,
                "buffer {i} gets memory"
            );
        }
    }
    check!(run_source(&s, 0, BUFFERS) == 0, "one frame for each buffer");
    for n in 0..BUFFERS {
        check!(
            wait_frame(&mut s, &mut f, 5000) && f.buffer < BUFFERS,
            "frame {n} arrived"
        );
        if f.buffer >= BUFFERS {
            continue;
        }
        let i = f.buffer as usize;
        check!(
            o[i].word(0) == f.sequence && o[i].word(bell_word) == f.sequence,
            "buffer {}'s frame is in buffer {}'s own dma-buf (content {}, sequence {})",
            f.buffer,
            f.buffer,
            o[i].word(0),
            f.sequence
        );
        seen |= 1 << f.buffer;
    }
    check!(
        seen == (1u32 << BUFFERS) - 1,
        "every buffer was filled once ({seen:x})"
    );

    // The consumer holds them all. Buffer 1 gets new memory before it goes back.
    o[1].fill(0x22, BUFFER_BYTES as usize);
    fresh.fill(0x33, BUFFER_BYTES as usize);
    let mut out = Buffer::default();
    check!(
        set_buffer(&s, 1, fresh.raw(), Some(&mut out)) == 0
            && same_place(e.name, out.addr, fresh.addr),
        "a buffer the consumer holds is given new memory"
    );
    for i in 0..BUFFERS {
        give_back(&mut s, i);
    }
    check!(run_source(&s, 0, BUFFERS) == 0, "four more frames");
    for n in 0..BUFFERS {
        if !wait_frame(&mut s, &mut f, 5000) {
            check!(false, "frame {n} after the swap");
            break;
        }
        if f.buffer == 1 {
            check!(
                fresh.word(0) == f.sequence && fresh.word(bell_word) == f.sequence,
                "buffer 1's next frame is in its new memory (content {}, sequence {})",
                fresh.word(0),
                f.sequence
            );
            swapped = 1;
        }
        give_back(&mut s, f.buffer);
    }
    check!(
        swapped != 0 && o[1].byte(0) == 0x22 && o[1].byte(BUFFER_BYTES as usize - 1) == 0x22,
        "and the memory it had is not written again ({:x})",
        o[1].byte(0)
    );
    check!(
        attached(e.name) == before + BUFFERS as i32,
        "one pin a buffer, the old one's gone"
    );

    // Swapping under a running source: each call either swaps (the buffer was free or with us) or is
    // refused because the source had it. Nothing else.
    check!(kick(&s) == 0, "the stream is sound before the race");
    // The source on one CPU and the swaps on another, so they really overlap.
    pin(1);
    check!(run_source(&s, 0, u32::MAX) == 0, "the source runs flat out");
    pin(0);
    for round in 0..20000u32 {
        let target = if round & 1 == 1 {
            fresh.raw()
        } else {
            other.raw()
        };
        let ret = set_buffer(&s, 1, target, None);
        swapped += i32::from(ret == 0);
        busy += i32::from(ret == -EBUSY);
        odd += i32::from(ret != 0 && ret != -EBUSY);
        // The source sleeps when it has no buffer: keep it fed.
        while pop_frame(&mut s, &mut f) {
            give_back(&mut s, f.buffer);
        }
    }
    check!(
        odd == 0 && swapped > 1,
        "20000 swaps under the source: {} swapped, {busy} busy, {odd} anything else",
        swapped - 1
    );
    let _ = guest::set_cpu_affinity(None);
    check!(run_source(&s, 0, 0) == 0, "the source stops");
    while pop_frame(&mut s, &mut f) {
        give_back(&mut s, f.buffer);
    }
    check!(
        kick(&s) == 0 && status(&s).state == STATE_OPEN,
        "and the stream is still sound"
    );

    // Closed with frames in flight: every buffer's pin goes with the stream.
    check!(run_source(&s, 0, u32::MAX) == 0, "running again");
    drop(s);
    let left = attached(e.name) - before;
    check!(
        left == 0,
        "closing released every buffer's pin ({left} left)"
    );
    for m in &mut o {
        m.free();
    }
    fresh.free();
    other.free();
}

/// A job's checks: a job is accepted, its frames carry its identity, and a kick is never refused.
fn job_round_trip(e: &Env<'_>, outstanding: u32, hold: bool, hammer: bool, what: &str) {
    let Some(mut s) = e.open(&format!("{what}: open")) else {
        return;
    };
    let mut f = Frame::default();
    let mut next_post: u64 = 1;
    let mut next_seen: u64 = 1;
    let mut wrong: u32 = 0;
    let mut bad_kicks: u32 = 0;
    for b in 0..outstanding {
        post(&mut s, &Job::new(b, next_post, 0));
        next_post += 1;
    }
    check!(
        kick(&s) == 0 && status(&s).jobs == u64::from(outstanding),
        "{what}: first jobs accepted before frames start"
    );
    check!(run_source(&s, 0, u32::MAX) == 0, "{what}: the source runs");
    let mut child = -1;
    if hammer {
        child = privilege::fork().unwrap_or(-1);
        if child == 0 {
            hammer_kicks(&s);
        }
    }
    let mut n = 0;
    while n < JOB_ROUNDS {
        if !wait_frame(&mut s, &mut f, 5000) {
            break;
        }
        if f.identity != next_seen {
            wrong += 1;
        }
        next_seen += 1;
        give_back(&mut s, f.buffer);
        if hold {
            if kick(&s) != 0 {
                bad_kicks += 1;
            }
            if n % 4 == 0 {
                sleep_ms(3);
            }
        }
        post(&mut s, &Job::new(f.buffer, next_post, 0));
        next_post += 1;
        n += 1;
    }
    reap(child);
    let st = status(&s);
    check!(
        n == JOB_ROUNDS,
        "{what}: {n} frames, returning a buffer and posting a job on it"
    );
    check!(
        wrong == 0,
        "{what}: every frame carried its job's identity ({wrong} did not)"
    );
    check!(bad_kicks == 0, "{what}: no kick refused ({bad_kicks} were)");
    check!(
        st.state == STATE_OPEN && st.fault == 0 && st.frames >= u64::from(JOB_ROUNDS),
        "{what}: never faulted: state {}, errno {}, {} frames, {} jobs",
        st.state,
        st.fault,
        st.frames,
        st.jobs
    );
    check!(run_source(&s, 0, 0) == 0, "{what}: source stopped");
}

/// A forked process that KICKs flat out, to land a drain between a consumer's return and its post.
fn hammer_kicks(s: &Stream<'_>) -> ! {
    loop {
        let _ = guest::ioctl_bytes(s.fd.as_fd(), KICK, &mut [0u8; 0]);
    }
}

fn test_job_driven(e: &Env<'_>) {
    job_round_trip(e, 1, true, false, "one job, window held open");
    job_round_trip(e, 1, false, true, "one job, KICKs racing");
    job_round_trip(e, BUFFERS, false, true, "every buffer posted, KICKs racing");

    // No job queued: nothing, however long the source runs. Then one job: one frame.
    let Some(mut s) = e.open("open another") else {
        return;
    };
    let mut f = Frame::default();
    post(&mut s, &Job::new(1, 0x1111, 0));
    check!(kick(&s) == 0, "one job accepted");
    check!(run_source(&s, 0, u32::MAX) == 0, "the source runs");
    check!(
        wait_frame(&mut s, &mut f, 5000) && f.identity == 0x1111 && f.buffer == 1,
        "that job's frame"
    );
    give_back(&mut s, f.buffer);
    check!(kick(&s) == 0, "buffer returned");
    let before = status(&s).frames;

    sleep_ms(200);
    check!(
        status(&s).frames == before && !pop_frame(&mut s, &mut f) && status(&s).dropped == 0,
        "no job queued: no frame for 200 ms, with every buffer free"
    );
    post(&mut s, &Job::new(2, 0x2222, 0));
    check!(
        wait_frame(&mut s, &mut f, 5000) && f.identity == 0x2222 && f.buffer == 2,
        "a job posted: its frame appears (buffer {}, identity 0x{:x})",
        f.buffer,
        f.identity
    );
    drop(s);
}

/// A job the module refuses: a fresh stream, the job posted, the errno its kick gives.
fn one_bad_job(e: &Env<'_>, mut j: Job, rel: bool, want: i32, what: &str) {
    let mut s = match open_with(e.ctl, e.block, e.pool) {
        Ok(s) => s,
        Err(_) => {
            check!(false, "{what}: open");
            return;
        }
    };
    if rel {
        for r in j.range.iter_mut().take(j.count as usize) {
            r.addr = r.addr.wrapping_add(s.pool_addr);
        }
    }
    post(&mut s, &j);
    check!(kick(&s) == -want, "{what}: KICK {}", strerror(want));
    expect_fault(&s, want, what);
}

/// Each is a fresh stream, faulted by exactly one thing.
fn test_bad_jobs(e: &Env<'_>) {
    one_bad_job(
        e,
        Job::new(BUFFERS, 1, 0),
        true,
        EINVAL,
        "buffer number past the pool",
    );
    one_bad_job(
        e,
        Job::new(0, 2, JOB_RANGES as u32 + 1),
        false,
        EINVAL,
        "too many ranges",
    );
    one_bad_job(
        e,
        Job::new(0, 3, 1).with(0, 0, 0),
        true,
        EINVAL,
        "a zero-length range",
    );
    one_bad_job(
        e,
        Job::new(0, 4, 1).with(0, 0, 4).with(1, 1, 1),
        true,
        EINVAL,
        "a used-looking spare range",
    );
    one_bad_job(
        e,
        Job::new(0, 5, 1).with(0, POOL_BYTES + 8192, 8),
        true,
        EFAULT,
        "an address outside everything",
    );
    one_bad_job(
        e,
        Job::new(0, 6, 1).with(0, 0x1000, 8),
        false,
        EFAULT,
        "a stray low address",
    );
    one_bad_job(
        e,
        Job::new(0, 7, 1).with(0, 0, u64::from(BUFFER_BYTES) + 1),
        true,
        EFAULT,
        "one past the end of the buffer",
    );
    one_bad_job(
        e,
        Job::new(0, 8, 1).with(0, u64::from(BUFFER_BYTES), 1),
        true,
        EACCES,
        "one past the end: the next buffer",
    );
    one_bad_job(
        e,
        Job::new(BUFFERS - 1, 9, 1).with(0, POOL_BYTES - 1, 2),
        true,
        EFAULT,
        "one past the end of the pool",
    );
    one_bad_job(
        e,
        Job::new(0, 11, 1).with(0, u64::MAX - 7, 16),
        false,
        EOVERFLOW,
        "addr + len wraps",
    );
    one_bad_job(
        e,
        Job::new(0, 12, 1).with(0, 0, u64::MAX),
        true,
        EOVERFLOW,
        "a length that wraps from the pool",
    );
    one_bad_job(
        e,
        Job::new(0, 13, 1).with(0, STRIDE, 8),
        true,
        EACCES,
        "an address in another buffer",
    );

    // A registered dma-buf is bounded exactly the same way.
    let (s, region, aux_addr) = stream_with_aux(e.ctl, e.block, e.pool, e.aux);
    check!(region > 0, "a stream with a registered dma-buf");
    let Some(mut s) = s else {
        return;
    };
    let aux_end = aux_addr.wrapping_add(AUX_BYTES);
    post(
        &mut s,
        &Job::new(0, 14, 1).with(0, aux_end.wrapping_sub(16), 16),
    );
    check!(
        kick(&s) == 0,
        "the last bytes of a registered dma-buf: accepted"
    );
    post(
        &mut s,
        &Job::new(1, 15, 1).with(0, aux_end.wrapping_sub(16), 17),
    );
    expect_fault(&s, EFAULT, "one past the end of a registered dma-buf");
    drop(s);
}

fn test_ring_faults(e: &Env<'_>) {
    let Some(mut s) = e.open("open") else {
        return;
    };
    // A job length the slot can't hold, or just wrong.
    post_raw(&mut s, JOB_BYTES as u64 + 8, &Job::default().bytes());
    expect_fault(&s, EMSGSIZE, "a job length past the slot");
    drop(s);

    let Some(mut s) = e.open("open") else {
        return;
    };
    post_raw(&mut s, 8, &Job::default().bytes());
    expect_fault(&s, EMSGSIZE, "a job length too short");
    drop(s);

    // Heads that can't be.
    let Some(s) = e.open("open") else {
        return;
    };
    s.store(s.control + SHM_JOB_HEAD, u64::MAX);
    expect_fault(&s, ERANGE, "jobs head impossible");
    drop(s);

    let Some(s) = e.open("open") else {
        return;
    };
    s.store(s.control + SHM_JOB_HEAD, u64::from(JOB_SLOTS) + 1);
    expect_fault(&s, ERANGE, "jobs head one past the capacity");
    drop(s);

    // The frame head is the module's own count. A consumer that scribbles on it, or on the frame's
    // index, changes nothing: the module does not read either, and the stream carries on.
    let Some(s) = e.open("open") else {
        return;
    };
    s.store(SHM_W_HEAD, 1 << 40);
    s.store(SHM_LANE_HEAD, 1 << 40);
    check!(
        kick(&s) == 0 && status(&s).fault == 0,
        "a scribbled frame head and lane head: no fault"
    );
    drop(s);

    // The jobs' head and tail are the only words the consumer's jobs use: a tail it writes is ignored.
    let Some(s) = e.open("open") else {
        return;
    };
    s.store(s.control + SHM_JOB_TAIL, 7);
    check!(
        kick(&s) == 0 && status(&s).fault == 0,
        "a scribbled job tail: no fault"
    );
    drop(s);
}

/// One xorshift step of the scribblers' generator.
fn xorshift(x: &mut u64) {
    *x ^= *x << 13;
    *x ^= *x >> 7;
    *x ^= *x << 17;
}

/// A forked consumer that writes random words over the whole ring until it is killed.
fn scribble_ring(s: &Stream<'_>) -> ! {
    let words = s.words();
    let n = s.ring_bytes as u64 / 8;
    let mut x: u64 = 0x1234_5678_90ab_cdef ^ u64::from(std::process::id());
    loop {
        for _ in 0..64 {
            xorshift(&mut x);
            if let Some(w) = words.get((x % n) as usize) {
                w.store(x, Ordering::Relaxed);
            }
        }
    }
}

/// A forked consumer that writes random words over the job slots only, until it is killed. The
/// heads stay honest, so a job's words are read while they change.
fn scribble_slots(s: &Stream<'_>, round: u64) -> ! {
    let words = s.words();
    let base = (s.control + SHM_JOB_HEAD) as u64;
    let end = s.ring_bytes as u64 / 8;
    let mut x: u64 = 0xdead_beef_cafe_f00d ^ u64::from(std::process::id()) ^ round;
    let mut i: u32 = 0;
    loop {
        xorshift(&mut x);
        if let Some(w) = words.get((base + x % (end - base)) as usize) {
            w.store(x, Ordering::Relaxed);
        }
        if i.is_multiple_of(4096) {
            thread::sleep(Duration::from_micros(1));
        }
        i = i.wrapping_add(1);
    }
}

/// Random garbage over the whole ring while the source and KICK run.
fn test_racing_garbage(e: &Env<'_>) {
    let Some(s) = e.open("open") else {
        return;
    };
    check!(run_source(&s, 100, u32::MAX) == 0, "the source runs");
    let child = privilege::fork().unwrap_or(-1);
    if child == 0 {
        scribble_ring(&s);
    }
    // Whatever happens, kicks keep getting an answer.
    let mut errors = [0u32; 4097];
    for n in 0..20000u32 {
        let k = kick(&s);
        if (-4095..=0).contains(&k) {
            errors[(-k) as usize] += 1;
        }
        if n % 1024 == 0 {
            sleep_ms(1);
        }
    }
    reap(child);
    let st = status(&s);
    check!(
        st.state == STATE_OPEN || st.state == STATE_FAULT,
        "the module survived a scribbling consumer: state {}, errno {}, kicks ok {}",
        st.state,
        st.fault,
        errors[0]
    );
    drop(s);
}

/// The slots only: each round is a fresh stream, and whichever way the race goes, it must end in an
/// accepted job or a refusal, never anything else.
fn test_racing_slots(e: &Env<'_>) {
    let (mut accepted, mut faulted, mut other) = (0u32, 0u32, 0u32);
    for round in 0..200u64 {
        let Ok(mut s) = open_with(e.ctl, e.block, e.pool) else {
            check!(false, "open round {round}");
            return;
        };
        run_source(&s, 1, u32::MAX);
        let child = privilege::fork().unwrap_or(-1);
        if child == 0 {
            scribble_slots(&s, round);
        }
        for i in 0..40u32 {
            let slot = u64::from(i & (BUFFERS - 1));
            let j = Job::new(i & (BUFFERS - 1), u64::from(i), 1).with(
                0,
                s.pool_addr + slot * STRIDE,
                8,
            );
            post(&mut s, &j);
            let k = kick(&s);
            if k == 0 {
                accepted += 1;
            } else if matches!(
                -k,
                EFAULT | EINVAL | EBUSY | EACCES | EOVERFLOW | EPERM | ERANGE | EMSGSIZE
            ) {
                faulted += 1;
            } else {
                other += 1;
            }
        }
        reap(child);
        drop(s);
    }
    check!(
        other == 0,
        "racing the slots: {accepted} jobs accepted, {faulted} refused, {other} anything else"
    );
}

/// The source must be running for the buffer to be held and for tails to be read: the frames, the
/// doorbell, the registration, the jobs that name a registered range, and the double posting.
fn test_with_frames(e: &Env<'_>, pool_cpu: &Mapping, aux_cpu: &Mapping) {
    let mut f = Frame::default();
    let bell = eventfd::create().ok();
    let opened = bell
        .as_ref()
        .map(|b| open_bell(e.ctl, e.block, e.pool, b.as_fd()));
    check!(
        bell.is_some() && matches!(opened, Some(Ok(_))),
        "stream open with a doorbell"
    );
    let Some(Ok(mut s)) = opened else {
        return;
    };
    let pool_page = shm_page_at(FRAME_SLOTS as usize);
    check!(
        s.load(0) == RING_MAGIC
            && s.load(1) == 2
            && s.load(2) == (1 | (u64::from(FRAME_SLOTS) << 32))
            && s.load(3) == 48
            && s.load(4) == pool_page as u64
            && s.load(SHM_W_END) == STATE_OPEN,
        "ring header is the shm layout 2"
    );
    check!(
        s.load(pool_page) == POOL_MAGIC
            && s.load(pool_page + 1) == 2
            && s.load(pool_page + 2) == (1 | (u64::from(BUFFERS) << 32)),
        "pool page is layout 2 for {BUFFERS} buffers"
    );
    check!(
        poll_one(s.fd.as_fd(), POLLIN, Some(Duration::ZERO)).unwrap_or(0) == 0,
        "poll: nothing waiting yet"
    );
    let before = now_ns();
    let mut last = before;

    check!(run_source(&s, 0, 8) == 0, "test source: 8 frames");
    // The source runs flat out and the module reuses a buffer no reader holds, so a consumer that is
    // slower than the source loses frames (shm/README.md: a reader that falls behind skips ahead).
    // Each frame delivered must still be whole and in order; delivered and dropped together are the 8
    // published. Nothing here depends on how fast the consumer runs.
    let mut delivered: u32 = 0;
    let mut prev_seq: u64 = 0;
    let mut seen = false;
    let mut tries = 0;
    while tries < 400 && u64::from(delivered) + s.dropped < 8 {
        tries += 1;
        if !wait_frame(&mut s, &mut f, 50) {
            continue;
        }
        let after = now_ns();
        check!(
            f.timestamp_ns >= last && f.timestamp_ns <= after,
            "frame {} stamped on CLOCK_MONOTONIC: {} in [{last}, {after}]",
            f.sequence,
            f.timestamp_ns
        );
        last = last.max(f.timestamp_ns);
        let base = f.buffer as usize * STRIDE as usize;
        let w0 = map_u64(pool_cpu, base);
        let wl = map_u64(pool_cpu, base + BUFFER_BYTES as usize - 8);
        check!(
            f.sequence < 8
                && (!seen || f.sequence > prev_seq)
                && f.buffer < BUFFERS
                && f.bytes == BUFFER_BYTES
                && w0 == f.sequence
                && wl == f.sequence
                && f.fence != 0,
            "frame {}: buffer {}, {} bytes, fence {}, content {}",
            f.sequence,
            f.buffer,
            f.bytes,
            f.fence,
            w0
        );
        prev_seq = f.sequence;
        seen = true;
        delivered += 1;
        give_back(&mut s, f.buffer);
    }
    check!(
        u64::from(delivered) + s.dropped == 8 && delivered >= 1,
        "every published frame is delivered or dropped: {delivered} delivered, {} dropped",
        s.dropped
    );
    let mut waited = 0;
    while waited < 5000 && status(&s).frames < 8 {
        sleep_ms(10);
        waited += 10;
    }
    check!(status(&s).frames == 8, "8 frames counted");
    // The counter is non-blocking: a read with nothing left says EAGAIN, and the drains are the rest.
    let count = bell.as_ref().map_or(0, |b| eventfd::drain(b.as_fd()));
    check!(
        s.drained + count >= 1,
        "doorbell rang {} times",
        s.drained + count
    );
    // Ends with every buffer given back: a job refused here (or below) must be for its own reason,
    // not because a buffer is still out.
    check!(kick(&s) == 0, "the good consumer is not refused");

    // A job: names a buffer and two ranges, comes back as this frame.
    let (r, reg) = register(&s, e.aux);
    check!(
        r.is_ok() && reg.region != 0 && reg.size >= AUX_BYTES,
        "registered a dma-buf: region {} at 0x{:x}",
        reg.region,
        reg.addr
    );
    let j = Job::new(2, 0xfeed_face_cafe_beef, 2)
        .with(0, reg.addr + 4096, 100)
        .with(1, s.pool_addr + 2 * STRIDE + 512, 16);
    fill(aux_cpu, 0, AUX_BYTES as usize);
    post(&mut s, &j);
    check!(
        kick(&s) == 0 && status(&s).jobs == 1,
        "the good job accepted"
    );
    check!(
        s.load(s.control + SHM_JOB_TAIL) == 1,
        "the ring saw it taken"
    );
    check!(run_source(&s, 0, 1) == 0, "the source fills it");
    check!(wait_frame(&mut s, &mut f, 5000), "the job's frame arrived");
    check!(
        f.buffer == 2 && f.identity == j.identity,
        "the frame is that job's: buffer {}, identity 0x{:x}",
        f.buffer,
        f.identity
    );
    let bad = (0..AUX_BYTES as usize)
        .filter(|&i| {
            let want = if (4096..4196).contains(&i) {
                PATTERN
            } else {
                0
            };
            map_u8(aux_cpu, i) != want
        })
        .count();
    check!(
        bad == 0,
        "the source wrote exactly the range it was given ({bad} bytes differ)"
    );
    let in_pool = 2 * STRIDE as usize;
    check!(
        map_u8(pool_cpu, in_pool + 512) == (PATTERN ^ 1)
            && map_u8(pool_cpu, in_pool + 527) == (PATTERN ^ 1)
            && map_u64(pool_cpu, in_pool) == f.sequence,
        "the source wrote the second range into its own buffer"
    );

    // Unregister while a job uses it: refused.
    let j = Job::new(1, 1, 1).with(0, reg.addr, 8);
    post(&mut s, &j);
    check!(kick(&s) == 0, "a job holds the region");
    check!(
        unregister(&s, reg.region) == Err(EBUSY),
        "unregister while in use: EBUSY"
    );

    // The job finishes, then it can go.
    check!(run_source(&s, 0, 1) == 0, "the source runs it");
    give_back(&mut s, f.buffer); // buffer 2
    check!(wait_frame(&mut s, &mut f, 5000), "its frame arrived");
    give_back(&mut s, f.buffer);
    check!(kick(&s) == 0, "both returned");
    check!(
        unregister(&s, reg.region) == Ok(()),
        "unregister after it finished"
    );
    check!(
        unregister(&s, reg.region) == Err(ENOENT),
        "unregister again: ENOENT"
    );
    check!(
        unregister(&s, 0) == Err(EINVAL),
        "the pool can't be unregistered"
    );
    drop(s);
    drop(bell);

    // Double posting: a buffer named twice before either runs.
    let Some(mut s) = e.open("open") else {
        return;
    };
    let a = Job::new(1, 1, 0);
    post(&mut s, &a);
    check!(kick(&s) == 0, "first posting of buffer 1 accepted");
    post(&mut s, &a);
    expect_fault(&s, EBUSY, "double posting");
    drop(s);

    // A buffer the consumer still holds.
    let Some(mut s) = e.open("open") else {
        return;
    };
    check!(
        run_source(&s, 0, 1) == 0,
        "one frame for the consumer to hold"
    );
    check!(wait_frame(&mut s, &mut f, 5000), "the consumer has it");
    check!(
        hold_buffer(&mut s, f.buffer) == 0,
        "the consumer holds the buffer (again)"
    );
    let h = Job::new(f.buffer, 1, 0);
    post(&mut s, &h);
    expect_fault(&s, EACCES, "a job on a buffer the consumer holds");
    drop(s);
}

/// Closing with a frame being made and jobs queued.
fn test_close_in_flight(e: &Env<'_>) {
    for round in 0..32 {
        let Ok(mut s) = open_with(e.ctl, e.block, e.pool) else {
            check!(false, "open round {round}");
            return;
        };
        post(&mut s, &Job::new(0, 1, 0));
        run_source(&s, 0, u32::MAX);
        drop(s);
    }
    check!(true, "32 closes with frames in flight");
}

/// No job at all, so the source keeps taking free buffers until the close lands.
fn test_close_jobless_in_flight(e: &Env<'_>) {
    for round in 0..32 {
        let Ok(s) = open_with(e.ctl, e.block, e.pool) else {
            check!(false, "open jobless round {round}");
            return;
        };
        run_source(&s, 0, u32::MAX);
        drop(s);
    }
    check!(true, "32 closes with jobless frames in flight");
}

/// Another stream, unrelated to the ones before: faults stay on their own stream.
fn test_another_stream(e: &Env<'_>) {
    let pool2 = alloc_pool(e.ctl, e.block, POOL_BYTES);
    let s = pool2
        .as_ref()
        .and_then(|p| open_with(e.ctl, e.block, p.as_raw_fd()).ok());
    check!(pool2.is_some() && s.is_some(), "another stream");
    let Some(mut s) = s else {
        return;
    };
    let mut f = Frame::default();
    check!(
        run_source(&s, 0, 2) == 0 && wait_frame(&mut s, &mut f, 5000),
        "frames still flow on a stream that broke no rules"
    );
    give_back(&mut s, f.buffer);
    check!(wait_frame(&mut s, &mut f, 5000), "and keep flowing");
    check!(kick(&s) == 0, "it isn't faulted");
    drop(s);
    drop(pool2);
}

/// A consumer that dies with the source running. The ring and its pin end with it.
fn test_crash(e: &Env<'_>) {
    let piped = std::io::pipe();
    check!(piped.is_ok(), "pipe");
    let Ok((mut reader, writer)) = piped else {
        return;
    };
    let child = privilege::fork().unwrap_or(-1);
    if child == 0 {
        let s = open_with(e.ctl, e.block, e.pool);
        let ok = s.as_ref().is_ok_and(|s| run_source(s, 100, u32::MAX) == 0);
        let mut writer = writer;
        let _ = writer.write_all(&[u8::from(ok)]);
        // Holds the stream until it is killed, as the C pause() does.
        loop {
            sleep_ms(3_600_000);
        }
    }
    drop(writer);
    let mut ready = [0u8; 1];
    let got = reader.read(&mut ready);
    check!(
        matches!(got, Ok(1)) && ready[0] != 0,
        "a process holds a running stream"
    );
    drop(reader);
    sleep_ms(50);
    if child > 0 {
        let _ = guest::kill(child, SIGKILL);
    }
    let end = if child > 0 {
        privilege::wait_child(child).ok()
    } else {
        None
    };
    check!(
        matches!(end, Some(ChildEnd::Signal(_))),
        "that process killed"
    );
}

/// The block unbound with a stream open: the stream ends, its pin of the block's memory goes, the
/// file stays and says so.
fn test_unbind(e: &Env<'_>, unbind: &str) {
    let fresh = alloc_pool(e.ctl, e.block, POOL_BYTES);
    let opened = fresh
        .as_ref()
        .and_then(|p| open_with(e.ctl, e.block, p.as_raw_fd()).ok());
    check!(
        fresh.is_some() && opened.is_some(),
        "a stream before the unbind"
    );
    let Some(s) = opened else {
        return;
    };
    check!(run_source(&s, 100, u32::MAX) == 0, "running");
    // Only the stream's pin holds it now.
    drop(fresh);

    // And one with buffers of their own, each held by nothing but its pin.
    let Ok(own) = make(e.ctl, &Layout::own(e.block), None) else {
        check!(false, "a stream with no pool before the unbind");
        return;
    };
    check!(true, "a stream with no pool before the unbind");
    for i in 0..BUFFERS {
        let mut m = own_alloc(e.ctl, e.block, u64::from(BUFFER_BYTES));
        check!(
            set_buffer(&own, i, m.raw(), None) == 0,
            "its buffer {i} has memory"
        );
        m.free();
    }
    check!(run_source(&own, 100, u32::MAX) == 0, "running too");

    let child = privilege::fork().unwrap_or(-1);
    if child == 0 {
        let wrote = OpenOptions::new()
            .write(true)
            .open(unbind)
            .ok()
            .is_some_and(|mut f| f.write(e.name.as_bytes()).is_ok_and(|n| n == e.name.len()));
        guest::exit_now(if wrote { 0 } else { 1 });
    }
    let end = if child > 0 {
        privilege::wait_child(child).ok()
    } else {
        None
    };
    check!(
        matches!(end, Some(ChildEnd::Exit(0))),
        "the unbind finished with the stream's pin released"
    );
    check!(
        kick(&s) == -ENODEV,
        "the stream's file says the block went: ENODEV"
    );
    check!(
        status(&s).state == STATE_CLOSED && s.load(SHM_W_END) == STATE_CLOSED,
        "the stream reads closed"
    );
    let ready = poll_one(s.fd.as_fd(), POLLIN, Some(Duration::ZERO)).unwrap_or(0);
    check!(ready != 0 && ready & POLLHUP != 0, "and poll says it ended");
    drop(s);
    check!(
        status(&own).state == STATE_CLOSED,
        "the stream with no pool ended too"
    );
    check!(
        set_buffer(&own, 0, e.aux, None) == -ENODEV,
        "and takes no memory once the block went: ENODEV"
    );
    let left = attached(e.name);
    check!(left == 0, "nothing is left attached to the block ({left})");
    drop(own);
}

/// The no-source run: only the checks that need no frames.
fn run_no_source(e: &Env<'_>, node: &str) -> ExitCode {
    let label = format!("{node} (no source)");
    test_refusals_at_create(e);
    test_own_refusals(e);
    let Some(mut s) = e.open("a stream without the test source") else {
        return done(&label);
    };
    check!(
        run_source(&s, 0, 1) == -EPERM,
        "the test source is off: EPERM"
    );
    let j = Job::new(0, 1, 1).with(0, 0x10, 8);
    post(&mut s, &j);
    check!(
        kick(&s) == -EFAULT,
        "a bad address refused without the test source: EFAULT"
    );
    drop(s);
    done(&label)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        let _ = writeln!(
            io::stderr(),
            "usage: lemnos_stream_check /dev/lemnos <name> <unbind path> <heap> [no-source]"
        );
        return ExitCode::from(2);
    }
    let (node, name, unbind, heap_path) = (&args[1], &args[2], &args[3], &args[4]);
    let no_source = args.len() > 5 && args[5] == "no-source";

    let ctl = match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(node)
    {
        Ok(f) => OwnedFd::from(f),
        Err(e) => {
            let _ = writeln!(io::stderr(), "{node}: {}", strerror(errno_of(&e)));
            return ExitCode::from(2);
        }
    };

    let block = module::find_block(ctl.as_fd(), name)
        .ok()
        .flatten()
        .unwrap_or(0);
    check!(block != 0, "block {name} is number {block}");

    let pool = alloc_pool(ctl.as_fd(), block, POOL_BYTES);
    check!(pool.is_some(), "pool allocated");
    let Some(pool) = pool else {
        return done(node);
    };
    let pool_cpu = Mapping::new(pool.as_fd(), POOL_BYTES as usize, 0, false);
    check!(pool_cpu.is_ok(), "pool mapped read-only");
    let Ok(pool_cpu) = pool_cpu else {
        return done(node);
    };

    let heap = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(heap_path)
        .map(OwnedFd::from);
    let aux = heap
        .as_ref()
        .ok()
        .and_then(|h| dma_heap::allocate(h.as_fd(), AUX_BYTES).ok());
    check!(
        heap.is_ok() && aux.is_some(),
        "a second dma-buf from the heap"
    );
    let (Ok(heap), Some(aux)) = (heap, aux) else {
        return done(node);
    };
    let aux_cpu = Mapping::new(aux.as_fd(), AUX_BYTES as usize, 0, true);
    check!(aux_cpu.is_ok(), "and mapped");
    let Ok(aux_cpu) = aux_cpu else {
        return done(node);
    };

    let env = Env {
        ctl: ctl.as_fd(),
        block,
        name: name.as_str(),
        pool: pool.as_raw_fd(),
        aux: aux.as_raw_fd(),
        heap: heap.as_fd(),
    };

    if no_source {
        return run_no_source(&env, node);
    }

    test_refusals_at_create(&env);
    test_register(&env);
    test_limits(&env);
    test_own_refusals(&env);
    test_own_frames(&env);
    test_with_frames(&env, &pool_cpu, &aux_cpu);
    test_job_driven(&env);
    test_bad_jobs(&env);
    test_ring_faults(&env);
    test_racing_garbage(&env);
    test_racing_slots(&env);
    test_another_stream(&env);
    test_close_in_flight(&env);
    test_close_jobless_in_flight(&env);
    test_crash(&env);

    // Streams are gone: the pool is ours alone again, and its one more release frees it.
    drop(pool_cpu);
    drop(pool);

    test_unbind(&env, unbind);

    drop(aux_cpu);
    drop(aux);
    drop(heap);
    drop(ctl);
    done(node)
}
