//! The VIP9000 check, against the fake block (test block `lemnos-test-vip`, module parameter
//! `test_vip_fake_regs`). The fake runs the command buffer the module built, through the tables the
//! module built, as the block's MMU would: so where it could write is where the module mapped.
//!
//! Programs: every command a program may not hold is refused at load with its offset. Jobs: every way
//! a job can name memory it doesn't hold is refused, ending the stream with the exact errno, before the
//! block runs anything. Then the MMU: a program that reaches past its slot, into a slot it didn't name,
//! into a slot of the job before, into its own commands, or nowhere, faults; the frame is published
//! with 0 bytes, nothing outside the job's memory changed, the block is reset and the next job runs. A
//! job that never ends is stopped the same way. A block that doesn't reset is wedged.
//!
//! Usage: `lemnos-vip-check /dev/lemnos <device> <heap> <run|command|board|wedge>` (init runs it as
//! `/lemnos_vip_check`). `command` is run with the fake reporting a chip pointed at its tables by a
//! command. `board` is for a real block: the load and job refusals and a job of NOPs, nothing that
//! needs the fake's own states.
//!
//! Port of `kernel/test/lemnos_vip_check.c` (CZ-GUEST-4), in the shared-memory ring form of
//! `kernel/rs/stream_ring.rs` (I2b2-6). The uAPI structures are laid out in bytes, sized as the C
//! structures are: `lemnos_vip_create` (64 bytes, with the stream's `lemnos_stream_create` at 56 of
//! it), `lemnos_vip_job` (232), `lemnos_vip_load` (24), `lemnos_vip_unload` (8), `lemnos_vip_status`
//! (48), and the stream's `lemnos_stream_register` (24) and `lemnos_stream_status` (40).
//!
//! Departures from the C, each on purpose:
//! - A FAIL line's "(line N)" is this file's line, not the C's line.
//! - Where the C would go on with a stream that never opened (the reopen after close), the Rust prints
//!   the FAIL line and skips that run's remaining good copy; the summary still prints.
//! - The C's `ran` values are all 0xffffffff when the attribute can't be read or parsed (its memset);
//!   the same here.
//! - A ring or image word outside its mapping reads as 0 and a write there is dropped, where the C
//!   would read or write past the mapping. No check reaches one in a passing run.
//! - The frame's pair, sequence, time and end words are not read: nothing here checks them.
//! - The descriptors are closed by drop, in the C's order.
//! - The FAIL text of the wedge's `put_sys` keeps the quotes the C's stringify escapes.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};
use std::thread;
use std::time::Duration;

use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::{dma_heap, guest, module};
use lemnos_linux_sys::{ioc, ior, iow, iowr};

// lemnos.h: the stream's requests, on the stream's file.
const KICK: u32 = ioc(0, b'L', 0x81, 0);
const REGISTER: u32 = iowr::<[u8; 24]>(b'L', 0x82);
const STREAM_STATUS: u32 = ior::<[u8; 40]>(b'L', 0x84);
// lemnos_vip.h: the block's requests, on the stream's file (its create is on the control node).
const VIP_LOAD: u32 = iowr::<[u8; 24]>(b'L', 0xe1);
const VIP_UNLOAD: u32 = iow::<[u8; 8]>(b'L', 0xe2);
const VIP_STATUS: u32 = ior::<[u8; 48]>(b'L', 0xe3);
const _: () = assert!(iowr::<[u8; 64]>(b'L', 0xe0) == 0xc040_4ce0);
const _: () = assert!(VIP_LOAD == 0xc018_4ce1);
const _: () = assert!(VIP_UNLOAD == 0x4008_4ce2);
const _: () = assert!(VIP_STATUS == 0x8030_4ce3);

// lemnos.h: the ring's end word (LEMNOS_RING_STATE_*).
const RING_OPEN: u32 = 0;
const RING_FAULT: u32 = 2;

// kernel/rs/stream_ring.rs (layout 2): the word numbers of the frame ring and the job block.
const SHM_W_HEAD: usize = 8;
const SHM_INDEX: usize = 17;
const SHM_SLOT_WORDS: usize = 10;
const SHM_ENTRY: usize = 4;
const SHM_TICKETS: usize = 8;
const SHM_READERS: usize = 64;
const SHM_JOB_HEAD: usize = 2;
const SHM_JOB_SLOTS: usize = 4;

// lemnos_vip.h: the limits, the addresses the block sees, and the MMU kinds.
const VIP_PROGRAMS: u32 = 8;
const VIP_SLOTS: usize = 13;
const VIP_SLOT_SPAN: u64 = 1 << 28;
const VIP_MAX_PROGRAM_BYTES: u32 = 65535 * 8 - 64;
const VIP_PROGRAM_ADDR: u32 = 0x0010_0000;
const VIP_MMU_COMMAND: u32 = 1;
const VIP_MMU_PD: u32 = 2;
const fn vip_slot_addr(n: u32) -> u32 {
    (n + 1) << 28
}
const _: () = assert!(vip_slot_addr(12) == 0xd000_0000);

// lemnos_vip.h: the commands (the encodings rs/vip.rs cites) and the fake's own states.
const fn state(addr: u32, n: u32) -> u32 {
    (1 << 27) | (n << 16) | addr
}
const NOP: u32 = 3 << 27;
const END: u32 = 2 << 27;
const LINK: u32 = 8 << 27;
const STALL: u32 = 9 << 27;
const F_COPY: u32 = 0x5000;
const F_HANG: u32 = 0x5003;

// The check's own sizes (the C's #defines).
const BUFFERS: u32 = 2;
const BUFFER_BYTES: u32 = 4096;
const FRAME_SLOTS: u32 = 2;
const JOB_SLOTS: u32 = 2;
/// `sizeof(struct lemnos_vip_job)`.
const JOB_BYTES: usize = 232;
/// A job slot: its length word, then the job's words.
const JOB_WORDS: usize = 1 + JOB_BYTES / 8;
const REGION: u64 = 1 << 20;
const PAGE: u32 = 4096;
const SLOT_BYTES: u64 = 2 * PAGE as u64;

/// The job's slot `n` in the region: 0 at its start, 1 at 64K, 2 at 128K.
const fn slot_at(n: u32) -> u64 {
    n as u64 * 0x10000
}

const _: () = assert!(JOB_BYTES == 24 + 16 * VIP_SLOTS);

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

/// A FAIL line, counted.
fn fail(line: &str) {
    say(line);
    FAILURES.fetch_add(1, Ordering::Relaxed);
}

/// The C `check()`: a FAIL line with the condition's text and the line it is checked at.
fn check_at(ok: bool, what: &str, line: u32) {
    if !ok {
        fail(&format!("FAIL {what} (line {line})"));
    }
}

/// `CHECK(cond)`, with the C's text for the condition.
macro_rules! check {
    ($cond:expr, $what:expr) => {
        check_at($cond, $what, line!())
    };
}

/// `strerror`: the text of an errno, as the C check's C library gives it (0 is its "no error").
fn strerror(errno: i32) -> String {
    if errno == 0 {
        return "No error information".to_owned();
    }
    let text = io::Error::from_raw_os_error(errno).to_string();
    match text.rfind(" (os error ") {
        Some(at) => text[..at].to_owned(),
        None => text,
    }
}

fn errno_of(e: &io::Error) -> i32 {
    e.raw_os_error().unwrap_or(0)
}

/// 0 for a request that succeeded, else its errno.
fn errno_or_zero(r: io::Result<()>) -> i32 {
    r.map_or_else(|e| errno_of(&e), |()| 0)
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

/// `strtol(text, NULL, 10)`: the number at the start after spaces and an optional sign; 0 without one.
fn strtol(text: &[u8]) -> i64 {
    let mut i = 0;
    while i < text.len() && matches!(text[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
        i += 1;
    }
    let negative = text.get(i) == Some(&b'-');
    if matches!(text.get(i), Some(b'+' | b'-')) {
        i += 1;
    }
    let mut v: i64 = 0;
    while let Some(d) = text.get(i).filter(|d| d.is_ascii_digit()) {
        v = v.wrapping_mul(10).wrapping_add(i64::from(d - b'0'));
        i += 1;
    }
    if negative { -v } else { v }
}

/// The first place `needle` occurs in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------------------------------
// The device: the control node, the heap, the block's name, and the device's attributes.
// ---------------------------------------------------------------------------------------------

struct Ctx {
    ctl: File,
    heap: File,
    block: u32,
    /// `/sys/bus/platform/devices/<device>`: the fake block's attributes.
    sysdir: String,
    /// A real block: nothing from the fake's files.
    board: bool,
}

/// `put_sys`: writes `text` to the fake block's attribute `name`; false when that fails.
fn put_sys(c: &Ctx, name: &str, text: &str) -> bool {
    let Ok(mut f) = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(format!("{}/{name}", c.sysdir))
    else {
        return false;
    };
    f.write(text.as_bytes()).is_ok_and(|n| n == text.len())
}

/// What the fake has run (its `ran` attribute): jobs (MMU on), faults, TLB flushes, bytes copied and
/// bad commands.
#[derive(Clone, Copy)]
struct Ran {
    jobs: u32,
    faults: u32,
    flushes: u32,
    copied: u32,
    errors: u32,
}

/// `ran`: the fake's counts, all 0xffffffff when the attribute can't be read (the C's memset). The
/// attribute is "jobs faults resets flushes copied errors", and parsing stops at the first word that
/// isn't a number.
fn ran(c: &Ctx) -> Ran {
    let mut v = [u32::MAX; 6];
    let read = if c.board {
        None
    } else {
        File::open(format!("{}/ran", c.sysdir)).ok()
    };
    let mut text = [0u8; 127];
    let n = read.and_then(|mut f| f.read(&mut text).ok()).unwrap_or(0);
    if n > 0 {
        let text = String::from_utf8_lossy(&text[..n]);
        let mut words = text.split_ascii_whitespace();
        for slot in &mut v {
            match words.next().and_then(|t| t.parse::<u32>().ok()) {
                Some(x) => *slot = x,
                None => break,
            }
        }
    }
    Ran {
        jobs: v[0],
        faults: v[1],
        flushes: v[3],
        copied: v[4],
        errors: v[5],
    }
}

// ---------------------------------------------------------------------------------------------
// A stream on the block: its ring, its image region, and the requests on it.
// ---------------------------------------------------------------------------------------------

/// The C's `struct stream`. The mappings go first, as the C unmaps before it closes, then the
/// descriptors: the stream, the region the image is in, and the pool.
struct Stream {
    ring: Mapping,
    /// The region, mapped: the slots are at `slot_at(n)`.
    image: Mapping,
    fd: OwnedFd,
    region_fd: OwnedFd,
    _pool: OwnedFd,
    region_addr: u64,
    pool_addr: u64,
    chip: u32,
    mmu: u32,
    control: usize,
    jobs_at: usize,
    jobs_head: u64,
    frames_tail: u64,
}

impl Stream {
    /// The ring's words, as atomics.
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

/// `shm_page_at`: the pool page's first word, after the frame ring.
fn shm_page_at(frame_slots: u32) -> usize {
    SHM_INDEX + frame_slots as usize + frame_slots as usize * SHM_SLOT_WORDS
}

/// `shm_control`: the control block's first word, after the pool page's tickets and cut-off words.
fn shm_control(frame_slots: u32, buffers: u32) -> usize {
    shm_page_at(frame_slots) + SHM_TICKETS + buffers as usize + SHM_READERS
}

/// `stream_open`: a VIP stream on the block, with its pool and region made from the heap and the
/// region registered on it. The errno it failed with, when it fails.
fn stream_open(c: &Ctx) -> Result<Stream, i32> {
    // The C makes both dma-bufs before it looks at either, so the errno is the last failing one's.
    let pool = dma_heap::allocate(c.heap.as_fd(), u64::from(BUFFERS) * u64::from(BUFFER_BYTES))
        .map_err(|e| errno_of(&e));
    let region = dma_heap::allocate(c.heap.as_fd(), REGION).map_err(|e| errno_of(&e));
    let region = region?;
    let pool = pool?;

    // `struct lemnos_vip_create`: the stream's create (56 bytes), then the block's chip and MMU.
    let mut arg = [0u8; 64];
    put32(&mut arg, 0, c.block);
    put32(&mut arg, 8, BUFFERS);
    put32(&mut arg, 12, BUFFER_BYTES);
    put32(&mut arg, 16, FRAME_SLOTS);
    put32(&mut arg, 20, JOB_SLOTS);
    put32(&mut arg, 24, JOB_BYTES as u32);
    put_i32(&mut arg, 28, pool.as_raw_fd());
    put_i32(&mut arg, 32, -1);
    let fd = guest::vip_create(c.ctl.as_fd(), &mut arg).map_err(|e| errno_of(&e))?;
    let ring_bytes = get64(&arg, 40) as usize;
    let pool_addr = get64(&arg, 48);
    let chip = get32(&arg, 56);
    let mmu = get32(&arg, 60);
    // The ring is mapped, then the image, as the C does; the errno is the image's when it fails.
    let ring = Mapping::new(fd.as_fd(), ring_bytes, 0, true);
    let image = Mapping::new(region.as_fd(), REGION as usize, 0, true);
    let (ring, image) = match (ring, image) {
        (Ok(ring), Ok(image)) => (ring, image),
        (_, Err(e)) | (Err(e), _) => return Err(errno_of(&e)),
    };

    let mut reg = [0u8; 24];
    put_i32(&mut reg, 0, region.as_raw_fd());
    guest::ioctl_bytes(fd.as_fd(), REGISTER, &mut reg).map_err(|e| errno_of(&e))?;
    let region_addr = get64(&reg, 8);
    let control = shm_control(FRAME_SLOTS, BUFFERS);
    Ok(Stream {
        ring,
        image,
        fd,
        region_fd: region,
        _pool: pool,
        region_addr,
        pool_addr,
        chip,
        mmu,
        control,
        jobs_at: control + SHM_JOB_SLOTS,
        jobs_head: 0,
        frames_tail: 0,
    })
}

/// `struct lemnos_stream_range`, in a job's slot: 16 bytes.
#[derive(Clone, Copy, Default)]
struct Range {
    addr: u64,
    len: u64,
}

/// `struct lemnos_vip_job`, in its slot: 232 bytes.
#[derive(Clone, Copy, Default)]
struct Job {
    identity: u64,
    buffer: u32,
    program: u32,
    slots: u32,
    reserved: u32,
    slot: [Range; VIP_SLOTS],
}

impl Job {
    fn bytes(&self) -> [u8; JOB_BYTES] {
        let mut b = [0u8; JOB_BYTES];
        put64(&mut b, 0, self.identity);
        put32(&mut b, 8, self.buffer);
        put32(&mut b, 12, self.program);
        put32(&mut b, 16, self.slots);
        put32(&mut b, 20, self.reserved);
        for (i, r) in self.slot.iter().enumerate() {
            put64(&mut b, 24 + 16 * i, r.addr);
            put64(&mut b, 32 + 16 * i, r.len);
        }
        b
    }
}

/// `post`: the job in the next slot, then the consumer's head.
fn post(s: &mut Stream, j: &Job) {
    let at = s.jobs_at + (s.jobs_head & u64::from(JOB_SLOTS - 1)) as usize * JOB_WORDS;
    let bytes = j.bytes();
    s.store(at, JOB_BYTES as u64);
    for i in 0..JOB_BYTES / 8 {
        s.store(at + 1 + i, get64(&bytes, 8 * i));
    }
    s.jobs_head += 1;
    fence(Ordering::Release);
    s.store(s.control + SHM_JOB_HEAD, s.jobs_head);
}

/// `shm_frame_read`, for `wait_frame`: frame `seq`'s identity and bytes, read through its index and
/// checked against its stamp. None while the frame is not out, and when its slot holds another frame
/// (the C's 0 and -1; the check takes only a 1).
fn frame_at(s: &Stream, seq: u64) -> Option<(u64, u32)> {
    if s.load(SHM_W_HEAD) <= seq {
        return None;
    }
    let slot = (s.load(SHM_INDEX + (seq & u64::from(FRAME_SLOTS - 1)) as usize) >> 16) as usize;
    let at = SHM_INDEX + FRAME_SLOTS as usize + slot * SHM_SLOT_WORDS;
    let stamp = s.load(at);
    if stamp != (seq + 1) << 1 {
        return None;
    }
    let identity = s.load(at + SHM_ENTRY + 3);
    let fence_word = s.load(at + SHM_ENTRY + 4);
    if s.load(at) != stamp {
        return None;
    }
    Some((identity, fence_word as u32))
}

/// `wait_frame`: the next frame's bytes, its identity in `identity`; -1 when none comes in `ms`.
/// A buffer needs no giving back: with no reader marking it, the module takes it back when it needs it.
fn wait_frame(s: &mut Stream, identity: &mut u64, ms: u32) -> i64 {
    for _ in (0..ms).step_by(2) {
        if let Some((id, bytes)) = frame_at(s, s.frames_tail) {
            *identity = id;
            s.frames_tail += 1;
            return i64::from(bytes);
        }
        thread::sleep(Duration::from_micros(2000));
    }
    -1
}

/// `KICK`'s errno, or 0.
fn kick(s: &Stream) -> i32 {
    errno_or_zero(guest::ioctl_bytes(s.fd.as_fd(), KICK, &mut [0u8; 0]))
}

/// `unload`'s errno, or 0.
fn unload(s: &Stream, program: u32) -> i32 {
    let mut a = [0u8; 8];
    put32(&mut a, 0, program);
    errno_or_zero(guest::ioctl_bytes(s.fd.as_fd(), VIP_UNLOAD, &mut a))
}

/// The stream's status, as its request gives it (all 0xff when the request fails).
struct StreamStatus {
    state: u32,
    fault: i32,
}

fn status_of(s: &Stream) -> StreamStatus {
    let mut b = [0xffu8; 40];
    let _ = guest::ioctl_bytes(s.fd.as_fd(), STREAM_STATUS, &mut b);
    StreamStatus {
        state: get32(&b, 0),
        fault: get32(&b, 4) as i32,
    }
}

/// `struct lemnos_vip_status`: the block's counts, since it was bound (all 0xff when the request fails).
struct BlockStatus {
    wedged: u32,
    finished: u64,
    refused: u64,
    faults: u64,
    timeouts: u64,
    last_fault: u32,
    resets: u32,
}

fn block_status(s: &Stream) -> BlockStatus {
    let mut b = [0xffu8; 48];
    let _ = guest::ioctl_bytes(s.fd.as_fd(), VIP_STATUS, &mut b);
    BlockStatus {
        wedged: get32(&b, 0),
        finished: get64(&b, 8),
        refused: get64(&b, 16),
        faults: get64(&b, 24),
        timeouts: get64(&b, 32),
        last_fault: get32(&b, 40),
        resets: get32(&b, 44),
    }
}

/// `load`: the errno (0 when loaded), and the value the C's `out` gets: the program, or on a refusal
/// the offset refused.
fn load_with(s: &Stream, mut a: [u8; 24]) -> (i32, u32) {
    let r = errno_or_zero(guest::ioctl_bytes(s.fd.as_fd(), VIP_LOAD, &mut a));
    (r, if r != 0 { get32(&a, 16) } else { get32(&a, 12) })
}

/// `load` of the program at `data` (a user address), `bytes` long.
fn load_at(s: &Stream, data: u64, bytes: u32) -> (i32, u32) {
    let mut a = [0u8; 24];
    put64(&mut a, 0, data);
    put32(&mut a, 8, bytes);
    load_with(s, a)
}

/// `load` of the words `w`.
fn load(s: &Stream, w: &[u32], bytes: u32) -> (i32, u32) {
    load_at(s, w.as_ptr() as u64, bytes)
}

// ---------------------------------------------------------------------------------------------
// Programs, jobs and the fake's copy program.
// ---------------------------------------------------------------------------------------------

/// A program copying `len` bytes from block address `src` to `dst`, through the fake's states.
/// Returns its length in bytes.
fn copy_prog(w: &mut [u32], src: u32, dst: u32, len: u32) -> u32 {
    w[0] = state(F_COPY, 3);
    w[1] = src;
    w[2] = dst;
    w[3] = len;
    w[4] = NOP;
    w[5] = 0;
    6 * 4
}

/// A job of program `program`, with `slots` slots of `SLOT_BYTES` at `slot_at(n)` in the region.
fn job_of(s: &Stream, program: u32, slots: u32, identity: u64) -> Job {
    let mut j = Job {
        identity,
        program,
        slots,
        ..Default::default()
    };
    for i in 0..slots {
        j.slot[i as usize] = Range {
            addr: s.region_addr + slot_at(i),
            len: SLOT_BYTES,
        };
    }
    j
}

/// The region's bytes for a fill: `seed + i * 7 + (i >> 9)`.
fn fill(s: &mut Stream, seed: u8) {
    for (i, b) in s.image.as_mut_slice().iter_mut().enumerate() {
        let i = i as u32;
        *b = (u32::from(seed) + i * 7 + (i >> 9)) as u8;
    }
}

/// `run_job`: posts `job` and kicks, then waits up to `ms` for its frame. The frame's bytes (-1: none).
fn run_job(s: &mut Stream, job: &Job, ms: u32) -> i64 {
    post(s, job);
    let k = kick(s);
    if k != 0 {
        fail(&format!("FAIL kick: {}", strerror(k)));
        return -1;
    }
    let mut id = 0u64;
    let n = wait_frame(s, &mut id, ms);
    check!(id == job.identity, "id == job->identity");
    n
}

/// Whether the `len` bytes at region offsets `a` and `b` are equal (the C's `!memcmp`).
fn same_bytes(img: &[u8], a: u64, b: u64, len: u32) -> bool {
    let len = len as usize;
    let (a, b) = (a as usize, b as usize);
    img[a..a + len] == img[b..b + len]
}

// ---------------------------------------------------------------------------------------------
// The load checks.
// ---------------------------------------------------------------------------------------------

fn run_load(c: &Ctx) {
    let st = match stream_open(c) {
        Ok(st) => st,
        Err(e) => {
            fail(&format!("FAIL stream: {}", strerror(e)));
            return;
        }
    };
    say(&format!(
        "chip 0x{:08x}, MMU set up by {}",
        st.chip,
        if st.mmu == VIP_MMU_PD {
            "register"
        } else {
            "command"
        }
    ));
    check!(st.chip != 0, "st.chip != 0");
    check!(
        st.mmu == VIP_MMU_PD || st.mmu == VIP_MMU_COMMAND,
        "st.mmu == LEMNOS_VIP_MMU_PD || st.mmu == LEMNOS_VIP_MMU_COMMAND"
    );

    // Each refused at its offset: after one good NOP.
    let bad: [(u32, u32, i32, &str); 16] = [
        (LINK, 0x0010_0000, libc::EPERM, "LINK"),
        (END, 0, libc::EPERM, "END"),
        (7 << 27, 0, libc::EPERM, "WAIT"),
        (10 << 27, 0, libc::EPERM, "CALL"),
        (11 << 27, 0, libc::EPERM, "RETURN"),
        (13 << 27, 0, libc::EPERM, "CHIP_ENABLE"),
        (
            state(0x0061, 1),
            0xffff_ffff,
            libc::EPERM,
            "the MMU's control state",
        ),
        (
            state(0x006b, 1),
            0,
            libc::EPERM,
            "the page table array's state",
        ),
        (
            state(0x03ff, 2),
            0,
            libc::EPERM,
            "a load crossing into the control states",
        ),
        (state(0x0e01, 1), 4, libc::EPERM, "the module's event"),
        (
            state(0x0e00, 2),
            0,
            libc::EPERM,
            "a load covering the module's event",
        ),
        (state(0x0e12, 1), 0, libc::EPERM, "the MMU's set-up state"),
        (
            state(0x0e10, 3),
            0,
            libc::EPERM,
            "a load covering the MMU's set-up state",
        ),
        (
            state(0xffff, 2),
            0,
            libc::EPERM,
            "a load past the last state",
        ),
        (
            state(0x0e00, 0),
            0,
            libc::EPERM,
            "1024 states: through the event",
        ),
        (
            state(0x5000, 3),
            0,
            libc::EINVAL,
            "a load running past the program",
        ),
    ];
    let mut w = [0u32; 64];
    for (a, b, err, what) in bad {
        w[0] = NOP;
        w[1] = 0;
        w[2] = a;
        w[3] = b;
        let (got, at) = load(&st, &w, 16);
        if got != err || at != 8 {
            fail(&format!(
                "FAIL load {what}: {} at {at}, wanted {} at 8",
                strerror(got),
                strerror(err)
            ));
        }
    }
    check!(
        load(&st, &w, 0).0 == libc::EINVAL,
        "load(&st, w, 0, NULL) == EINVAL"
    );
    check!(
        load(&st, &w, 12).0 == libc::EINVAL,
        "load(&st, w, 12, NULL) == EINVAL"
    );
    check!(
        load(&st, &w, VIP_MAX_PROGRAM_BYTES + 8).0 == libc::EINVAL,
        "load(&st, w, LEMNOS_VIP_MAX_PROGRAM_BYTES + 8, NULL) == EINVAL"
    );
    check!(
        load_at(&st, 0, 8).0 == libc::EFAULT,
        "load(&st, NULL, 8, NULL) == EFAULT"
    );
    {
        let mut a = load_args(&w, 8);
        put32(&mut a, 20, 1);
        check!(
            errno_or_zero(guest::ioctl_bytes(st.fd.as_fd(), VIP_LOAD, &mut a)) == libc::EINVAL,
            "ioctl(st.fd, LEMNOS_VIP_IOC_LOAD, &a) && errno == EINVAL"
        );
    }

    // What may run: state loads past the control states, NOP, STALL.
    w[0] = state(0x0400, 1);
    w[1] = 0;
    w[2] = state(0x0e02, 1); // the semaphore, next to the event
    w[3] = 0x701;
    w[4] = STALL;
    w[5] = 0x701;
    w[6] = state(0x0e13, 3); // the states after the MMU's set-up, padded
    w[7] = 1;
    w[8] = 2;
    w[9] = 3;
    w[10] = NOP;
    w[11] = 0;
    let (got, out) = load(&st, &w, 48);
    check!(got == 0, "load(&st, w, 48, &out) == 0");
    check!(
        (1..=VIP_PROGRAMS).contains(&out),
        "out >= 1 && out <= LEMNOS_VIP_PROGRAMS"
    );

    // At most LEMNOS_VIP_PROGRAMS at once.
    for _ in 1..VIP_PROGRAMS {
        check!(load(&st, &w, 8).0 == 0, "load(&st, w, 8, NULL) == 0");
    }
    check!(
        load(&st, &w, 8).0 == libc::EBUSY,
        "load(&st, w, 8, NULL) == EBUSY"
    );
    check!(unload(&st, out) == 0, "unload(&st, out) == 0");
    check!(
        unload(&st, out) == libc::ENOENT,
        "unload(&st, out) == ENOENT"
    );
    check!(unload(&st, 0) == libc::EINVAL, "unload(&st, 0) == EINVAL");
    check!(
        unload(&st, VIP_PROGRAMS + 1) == libc::EINVAL,
        "unload(&st, LEMNOS_VIP_PROGRAMS + 1) == EINVAL"
    );
    let (got, at) = load(&st, &w, 8);
    check!(got == 0, "load(&st, w, 8, &at) == 0");
    check!(at == out, "at == out");
    check!(
        status_of(&st).state == RING_OPEN,
        "status_of(&st).state == LEMNOS_RING_STATE_OPEN"
    );
    drop(st);
}

/// The 24-byte `lemnos_vip_load` for the words `w`, `bytes` long.
fn load_args(w: &[u32], bytes: u32) -> [u8; 24] {
    let mut a = [0u8; 24];
    put64(&mut a, 0, w.as_ptr() as u64);
    put32(&mut a, 8, bytes);
    a
}

// ---------------------------------------------------------------------------------------------
// The refused jobs: each edit of a good job, refused with its errno, the block never run.
// ---------------------------------------------------------------------------------------------

/// One refusal's edit of a good job.
type Edit = fn(&Stream, &mut Job);

fn e_unregistered(s: &Stream, j: &mut Job) {
    j.slot[1].addr = s.region_addr + REGION;
}

fn e_past_region(s: &Stream, j: &mut Job) {
    j.slot[1].addr = s.region_addr + REGION - u64::from(PAGE);
}

fn e_wraps(_: &Stream, j: &mut Job) {
    j.slot[1].addr = u64::MAX - u64::from(PAGE) + 1;
}

fn e_pool(s: &Stream, j: &mut Job) {
    j.slot[1].addr = s.pool_addr;
    j.slot[1].len = u64::from(PAGE);
}

fn e_unaligned(_: &Stream, j: &mut Job) {
    j.slot[1].addr += 64;
}

fn e_part_page(_: &Stream, j: &mut Job) {
    j.slot[1].len = u64::from(PAGE) + 64;
}

fn e_empty(_: &Stream, j: &mut Job) {
    j.slot[1].len = 0;
}

fn e_too_long(_: &Stream, j: &mut Job) {
    j.slot[1].len = VIP_SLOT_SPAN + u64::from(PAGE);
}

fn e_too_many(_: &Stream, j: &mut Job) {
    j.slots = VIP_SLOTS as u32 + 1;
}

fn e_unused_slot(s: &Stream, j: &mut Job) {
    j.slot[5].addr = s.region_addr;
}

fn e_reserved(_: &Stream, j: &mut Job) {
    j.reserved = 1;
}

fn e_no_program(_: &Stream, j: &mut Job) {
    j.program = 0;
}

fn e_not_loaded(_: &Stream, j: &mut Job) {
    j.program = VIP_PROGRAMS;
}

fn e_bad_buffer(_: &Stream, j: &mut Job) {
    j.buffer = BUFFERS;
}

/// A job that must be refused with `err`, ending its stream, on a stream of its own.
fn refused(c: &Ctx, what: &str, err: i32, edit: Edit) {
    let mut st = match stream_open(c) {
        Ok(st) => st,
        Err(e) => {
            fail(&format!("FAIL {what}: stream ({})", strerror(e)));
            return;
        }
    };
    let mut w = [0u32; 8];
    w[0] = NOP;
    w[1] = 0;
    let (got, prog) = load(&st, &w, 8);
    check!(got == 0, "load(&st, w, 8, &prog) == 0");
    let mut job = job_of(&st, prog, 2, 0x77);
    edit(&st, &mut job);
    let before = ran(c);
    let b0 = block_status(&st);
    post(&mut st, &job);
    let got = kick(&st);
    if got != err {
        fail(&format!(
            "FAIL {what}: {}, wanted {}",
            strerror(got),
            strerror(err)
        ));
    }
    check!(
        status_of(&st).state == RING_FAULT,
        "status_of(&st).state == LEMNOS_RING_STATE_FAULT"
    );
    check!(status_of(&st).fault == err, "status_of(&st).fault == err");
    thread::sleep(Duration::from_micros(5000));
    // Nothing reached the block.
    check!(
        block_status(&st).finished == b0.finished,
        "block_status(&st).finished == b0.finished"
    );
    check!(
        block_status(&st).refused == b0.refused.wrapping_add(1),
        "block_status(&st).refused == b0.refused + 1"
    );
    let after = ran(c);
    check!(after.jobs == before.jobs, "after.jobs == before.jobs");
}

fn run_refusals(c: &Ctx) {
    refused(
        c,
        "a slot outside every registered buffer",
        libc::EFAULT,
        e_unregistered,
    );
    refused(
        c,
        "a slot running past its buffer",
        libc::EFAULT,
        e_past_region,
    );
    refused(c, "a slot whose end wraps", libc::EOVERFLOW, e_wraps);
    refused(c, "a slot in the pool", libc::EACCES, e_pool);
    refused(c, "a slot not on a page", libc::EINVAL, e_unaligned);
    refused(c, "a slot not whole pages", libc::EINVAL, e_part_page);
    refused(c, "an empty slot", libc::EINVAL, e_empty);
    refused(c, "a slot longer than its span", libc::EINVAL, e_too_long);
    refused(c, "more slots than there are", libc::EINVAL, e_too_many);
    refused(c, "an unused slot not zero", libc::EINVAL, e_unused_slot);
    refused(c, "a reserved field not zero", libc::EINVAL, e_reserved);
    refused(c, "program 0", libc::EINVAL, e_no_program);
    refused(c, "a program not loaded", libc::EINVAL, e_not_loaded);
    refused(
        c,
        "a pool buffer that isn't one",
        libc::EINVAL,
        e_bad_buffer,
    );
}

// ---------------------------------------------------------------------------------------------
// Jobs on the block.
// ---------------------------------------------------------------------------------------------

/// A program copying from `src` to `dst` that must fault, with the region otherwise untouched.
fn faults(c: &Ctx, st: &mut Stream, slots: u32, src: u32, dst: u32, len: u32, what: &str) {
    let mut before = vec![0u8; REGION as usize];
    let b0 = block_status(st);
    let r0 = ran(c);
    fill(st, 0x5a);
    before.copy_from_slice(st.image.as_slice());
    let mut w = [0u32; 8];
    let bytes = copy_prog(&mut w, src, dst, len);
    let (got, prog) = load(st, &w, bytes);
    check!(
        got == 0,
        "load(st, w, copy_prog(w, src, dst, len), &prog) == 0"
    );
    let j = job_of(st, prog, slots, 0xf00 + b0.faults);

    let n = run_job(st, &j, 3000);
    let b1 = block_status(st);
    let r1 = ran(c);
    if n != 0 || b1.faults != b0.faults.wrapping_add(1) || r1.faults != r0.faults.wrapping_add(1) {
        fail(&format!(
            "FAIL {what}: frame {n}, faults {} -> {}, fake faults {} -> {}",
            b0.faults, b1.faults, r0.faults, r1.faults
        ));
    }
    check!(
        (b1.last_fault & (1 << 30)) != 0,
        "b1.last_fault & (1u << 30)"
    );
    check!(b1.resets > b0.resets, "b1.resets > b0.resets");
    check!(b1.wedged == 0, "b1.wedged == 0");
    check!(
        status_of(st).state == RING_OPEN,
        "status_of(st).state == LEMNOS_RING_STATE_OPEN"
    );
    // Whatever the fake reached before the fault, nothing outside the job's slots changed.
    let img = st.image.as_slice();
    for i in 0..REGION {
        let in_slot = (0..slots).any(|s| i >= slot_at(s) && i < slot_at(s) + SLOT_BYTES);
        if !in_slot && img[i as usize] != before[i as usize] {
            fail(&format!("FAIL {what}: byte {i} of the region changed"));
            break;
        }
    }
    check!(unload(st, prog) == 0, "unload(st, prog) == 0");
}

/// A good copy: slot 0's first `len` bytes to slot 1, checked byte by byte.
fn good_copy(c: &Ctx, st: &mut Stream, len: u32, identity: u64) {
    let b0 = block_status(st);
    let r0 = ran(c);
    fill(st, identity as u8);
    let mut w = [0u32; 8];
    let bytes = copy_prog(&mut w, vip_slot_addr(0), vip_slot_addr(1), len);
    let (got, prog) = load(st, &w, bytes);
    check!(
        got == 0,
        "load(st, w, copy_prog(w, LEMNOS_VIP_SLOT_ADDR(0), LEMNOS_VIP_SLOT_ADDR(1), len), &prog) == 0"
    );
    let j = job_of(st, prog, 2, identity);
    let n = run_job(st, &j, 3000);
    if n != i64::from(BUFFER_BYTES) {
        fail(&format!("FAIL good copy {identity}: frame {n}"));
    }
    check!(
        block_status(st).finished == b0.finished.wrapping_add(1),
        "block_status(st).finished == b0.finished + 1"
    );
    check!(
        same_bytes(st.image.as_slice(), slot_at(1), slot_at(0), len),
        "!memcmp(st->image + SLOT_AT(1), st->image + SLOT_AT(0), len)"
    );
    let r1 = ran(c);
    check!(r1.jobs == r0.jobs.wrapping_add(1), "r1.jobs == r0.jobs + 1");
    check!(
        r1.copied == r0.copied.wrapping_add(len),
        "r1.copied == r0.copied + len"
    );
    // The module's preamble flushed the TLB first.
    check!(
        r1.flushes == r0.flushes.wrapping_add(1),
        "r1.flushes == r0.flushes + 1"
    );
    check!(r1.errors == 0, "r1.errors == 0");
    check!(unload(st, prog) == 0, "unload(st, prog) == 0");
}

fn run_jobs(c: &Ctx, command: bool) {
    let mut st = match stream_open(c) {
        Ok(st) => st,
        Err(e) => {
            fail(&format!("FAIL stream: {}", strerror(e)));
            return;
        }
    };
    let want_mmu = if command { VIP_MMU_COMMAND } else { VIP_MMU_PD };
    check!(
        c.board || st.mmu == want_mmu,
        "board || st.mmu == (command ? LEMNOS_VIP_MMU_COMMAND : LEMNOS_VIP_MMU_PD)"
    );

    // A job of NOPs: what a real block can run too.
    let mut w = [0u32; 8];
    w[0] = NOP;
    w[1] = 0;
    w[2] = STALL;
    w[3] = 0x701;
    let (got, prog) = load(&st, &w, 16);
    check!(got == 0, "load(&st, w, 16, &prog) == 0");
    let j = job_of(&st, prog, 1, 1);
    check!(
        run_job(&mut st, &j, 3000) == i64::from(BUFFER_BYTES),
        "run_job(&st, &j, 3000) == BUFFER_BYTES"
    );
    check!(
        block_status(&st).finished >= 1,
        "block_status(&st).finished >= 1"
    );
    check!(unload(&st, prog) == 0, "unload(&st, prog) == 0");

    if !c.board {
        good_copy(c, &mut st, PAGE, 2);
        good_copy(c, &mut st, 2 * PAGE, 3); // across a page of each slot

        faults(
            c,
            &mut st,
            2,
            vip_slot_addr(0),
            vip_slot_addr(1) + 2 * PAGE,
            64,
            "a write past the slot's end",
        );
        good_copy(c, &mut st, PAGE, 4); // recovered
        faults(
            c,
            &mut st,
            2,
            vip_slot_addr(0),
            vip_slot_addr(1) + PAGE,
            PAGE + 64,
            "a write running off the slot's end",
        );
        faults(
            c,
            &mut st,
            2,
            vip_slot_addr(0) - PAGE,
            vip_slot_addr(1),
            64,
            "a read before the first slot",
        );
        faults(
            c,
            &mut st,
            2,
            vip_slot_addr(0),
            vip_slot_addr(2),
            64,
            "a write to a slot the job didn't name",
        );
        faults(
            c,
            &mut st,
            2,
            vip_slot_addr(0),
            VIP_PROGRAM_ADDR,
            64,
            "a write to the job's own commands",
        );
        faults(
            c,
            &mut st,
            2,
            vip_slot_addr(0),
            0xf000_0000,
            64,
            "a write to nothing mapped",
        );
        faults(
            c,
            &mut st,
            2,
            0x0000_0000,
            vip_slot_addr(1),
            64,
            "a read of address 0",
        );

        // The job before named slot 2; this one doesn't, and can't reach it.
        let bytes = copy_prog(&mut w, vip_slot_addr(0), vip_slot_addr(2), PAGE);
        let (got, prog) = load(&st, &w, bytes);
        check!(
            got == 0,
            "load(&st, w, copy_prog(w, LEMNOS_VIP_SLOT_ADDR(0), LEMNOS_VIP_SLOT_ADDR(2), PAGE), &prog) == 0"
        );
        let j = job_of(&st, prog, 3, 5);
        let n = run_job(&mut st, &j, 3000);
        check!(n == i64::from(BUFFER_BYTES), "n == BUFFER_BYTES");
        check!(
            same_bytes(st.image.as_slice(), slot_at(2), slot_at(0), PAGE),
            "!memcmp(st.image + SLOT_AT(2), st.image + SLOT_AT(0), PAGE)"
        );
        check!(unload(&st, prog) == 0, "unload(&st, prog) == 0");
        faults(
            c,
            &mut st,
            2,
            vip_slot_addr(0),
            vip_slot_addr(2),
            64,
            "a write to the slot of the job before",
        );
        faults(
            c,
            &mut st,
            1,
            vip_slot_addr(1),
            vip_slot_addr(0),
            64,
            "a read of the slot of the job before",
        );

        // A job that never ends: stopped by a reset, published empty, and the stream goes on.
        {
            let b0 = block_status(&st);
            w[0] = state(F_HANG, 1);
            w[1] = 0;
            let (got, prog) = load(&st, &w, 8);
            check!(got == 0, "load(&st, w, 8, &prog) == 0");
            let j = job_of(&st, prog, 1, 6);
            check!(
                run_job(&mut st, &j, 5000) == 0,
                "run_job(&st, &j, 5000) == 0"
            );
            check!(
                block_status(&st).timeouts == b0.timeouts.wrapping_add(1),
                "block_status(&st).timeouts == b0.timeouts + 1"
            );
            check!(
                block_status(&st).resets > b0.resets,
                "block_status(&st).resets > b0.resets"
            );
            check!(
                block_status(&st).wedged == 0,
                "block_status(&st).wedged == 0"
            );
            check!(
                status_of(&st).state == RING_OPEN,
                "status_of(&st).state == LEMNOS_RING_STATE_OPEN"
            );
            check!(unload(&st, prog) == 0, "unload(&st, prog) == 0");
        }
        good_copy(c, &mut st, PAGE, 7);
        // Unloaded programs are gone.
        let k = job_of(&st, 1, 1, 8);
        post(&mut st, &k);
        check!(kick(&st) == libc::EINVAL, "kick(&st) == EINVAL");
    }

    // One stream at a time.
    check!(
        matches!(stream_open(c), Err(libc::EBUSY)),
        "stream_open(&second) && errno == EBUSY"
    );
    drop(st);

    // Closed: the block takes a stream again, with its programs gone.
    let reopened = stream_open(c);
    check!(reopened.is_ok(), "!stream_open(&st)");
    if let Ok(mut st) = reopened
        && !c.board
    {
        good_copy(c, &mut st, PAGE, 9);
    }
}

/// A block that doesn't reset: wedged, and its job's memory stays held.
fn run_wedge(c: &Ctx) {
    let mut st = match stream_open(c) {
        Ok(st) => st,
        Err(e) => {
            fail(&format!("FAIL stream: {}", strerror(e)));
            return;
        }
    };
    let w = [state(F_HANG, 1), 0];
    let (got, prog) = load(&st, &w, 8);
    check!(got == 0, "load(&st, w, 8, &prog) == 0");
    check!(
        put_sys(c, "stuck", "1"),
        "!put_sys(\\\"stuck\\\", \\\"1\\\")"
    );
    let j = job_of(&st, prog, 1, 1);
    post(&mut st, &j);
    check!(kick(&st) == 0, "kick(&st) == 0");
    let mut t = 0;
    while t < 400 && status_of(&st).state == RING_OPEN {
        thread::sleep(Duration::from_micros(5000));
        t += 1;
    }
    {
        let b = block_status(&st);
        let s = status_of(&st);
        if s.state != RING_FAULT || s.fault != libc::EIO || b.wedged != 1 {
            fail(&format!(
                "FAIL not wedged: state {} fault {}; wedged {} timeouts {}",
                s.state, s.fault, b.wedged, b.timeouts
            ));
        }
    }
    // The stream is gone, its memory isn't: the kernel still holds the region.
    let region = st.region_fd.try_clone().ok();
    drop(st);
    let counted = region.as_ref().and_then(|r| fdinfo_count(r.as_raw_fd()));
    check!(
        counted.is_some_and(|n| n > 1),
        "p && strtol(p + 6, NULL, 10) > 1"
    );
    drop(region);
    check!(
        matches!(stream_open(c), Err(libc::EIO)),
        "stream_open(&st) && errno == EIO"
    );
}

/// The `count:` of `/proc/self/fdinfo/<fd>`, the descriptor's references; None when it can't be read.
fn fdinfo_count(fd: i32) -> Option<i64> {
    let mut f = File::open(format!("/proc/self/fdinfo/{fd}")).ok()?;
    let mut text = [0u8; 255];
    let n = f.read(&mut text).ok()?;
    let text = &text[..n];
    let at = find(text, b"count:")? + 6;
    Some(strtol(&text[at..]))
}

// ---------------------------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------------------------

fn open_rw(path: &str) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(path)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        let name = args.first().map_or("lemnos_vip_check", String::as_str);
        let _ = writeln!(
            io::stderr(),
            "usage: {name} /dev/lemnos <device> <heap> <run|command|board|wedge>"
        );
        return ExitCode::from(2);
    }
    let (ctl, heap) = match (open_rw(&args[1]), open_rw(&args[3])) {
        (Ok(ctl), Ok(heap)) => (ctl, heap),
        (ctl, heap) => {
            // perror: the errno of the last failing open.
            let e = heap.err().or(ctl.err());
            let text = e.map_or_else(String::new, |e| strerror(errno_of(&e)));
            let _ = writeln!(io::stderr(), "open: {text}");
            return ExitCode::from(1);
        }
    };
    let block = module::find_block(ctl.as_fd(), &args[2])
        .ok()
        .flatten()
        .unwrap_or(0);
    if block == 0 {
        say(&format!("FAIL no block {}", args[2]));
        return ExitCode::from(1);
    }
    let mode = args[4].as_str();
    let c = Ctx {
        ctl,
        heap,
        block,
        sysdir: format!("/sys/bus/platform/devices/{}", args[2]),
        board: mode == "board",
    };
    match mode {
        "run" | "board" => {
            run_load(&c);
            run_refusals(&c);
            run_jobs(&c, false);
        }
        "command" => run_jobs(&c, true),
        "wedge" => run_wedge(&c),
        _ => return ExitCode::from(2),
    }
    say(&format!("lemnos_vip_check {mode}: {} failed", failures()));
    if failures() > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
