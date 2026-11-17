//! The graphics DMA check, against the K230 engine's fake (test block `lemnos-test-gsdma`, module
//! parameter `test_gsdma_fake_regs`). The fake walks the descriptors the module built and copies
//! through the bus addresses they name, so what it writes is what the module told it.
//!
//! Every field of a job that becomes memory the engine touches is tried out of bounds, wrapped,
//! overlapping and disagreeing with its format, on a job that is otherwise good, and the exact errno
//! is required. Good jobs are checked pixel by pixel, turned and not. A job that never finishes is
//! stopped and the stream goes on; an engine that never stops is wedged and keeps the job's memory.
//!
//! Usage: `lemnos-gsdma-check /dev/lemnos <device> <heap> <run|board|reach|wedge>` (init runs it as
//! `/lemnos_gsdma_check`). `board` is run for a real engine: the refusals (proved by the block's own
//! finished count staying put) and the good copies, without anything that needs the fake engine.
//!
//! Port of `kernel/test/lemnos_gsdma_check.c` (CZ-GUEST-4), in the shared-memory ring form of
//! `kernel/test/shm_ring.h` (I2b2-6). The uAPI structures are laid out in bytes, sized as the C
//! structures are: `lemnos_gsdma_create` (64 bytes), `lemnos_gsdma_job` (104), `lemnos_gsdma_status`
//! (32), and the stream's `lemnos_stream_create` (56, inside the create), `lemnos_stream_register`
//! (24) and `lemnos_stream_status` (40).
//!
//! Departures from the C, each on purpose:
//! - A FAIL line's "(line N)" is this file's line, not the C's line.
//! - Where the C would go on with a stream that never opened (a reopen after close, the reach run's
//!   stream), the Rust prints the FAIL line and gives up the rest of that run; the summary still prints.
//! - The C's `ran` values are read as zero where the C left them uninitialised (they show only on a
//!   FAIL line or in a check that already failed).
//! - `refused` takes no job argument: every caller built the job from the stream, as the C does
//!   with its NULL.
//! - The descriptors are closed by drop, not by the C's explicit closes; the order is the same.
//! - The frame's pair, sequence, time and end words are not read: nothing here checks them.

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
use lemnos_linux_sys::{ioc, ior, iowr};

// lemnos.h: the stream's requests, on the stream's file.
const KICK: u32 = ioc(0, b'L', 0x81, 0);
const REGISTER: u32 = iowr::<[u8; 24]>(b'L', 0x82);
const STREAM_STATUS: u32 = ior::<[u8; 40]>(b'L', 0x84);
// lemnos_gsdma.h: the block's status, on the stream's file.
const GSDMA_STATUS: u32 = ior::<[u8; 32]>(b'L', 0xe9);
const _: () = assert!(iowr::<[u8; 64]>(b'L', 0xe8) == 0xc040_4ce8);
const _: () = assert!(GSDMA_STATUS == 0x8020_4ce9);

// lemnos.h: the ring's end word (LEMNOS_RING_STATE_*).
const RING_OPEN: u32 = 0;
const RING_FAULT: u32 = 2;

// kernel/test/shm_ring.h (layout 2): the word numbers of the frame ring and the job block.
const SHM_W_HEAD: usize = 8;
const SHM_INDEX: usize = 17;
const SHM_SLOT_WORDS: usize = 10;
const SHM_ENTRY: usize = 4;
const SHM_TICKETS: usize = 8;
const SHM_READERS: usize = 64;
const SHM_JOB_HEAD: usize = 2;
const SHM_JOB_SLOTS: usize = 4;

// The check's own sizes (the C's #defines).
const BUFFERS: u32 = 2;
const BUFFER_BYTES: u32 = 4096;
const FRAME_SLOTS: u32 = 2;
const JOB_SLOTS: u32 = 2;
/// `sizeof(struct lemnos_gsdma_job)`.
const JOB_BYTES: usize = 104;
/// A job slot: its length word, then the job's words.
const JOB_WORDS: usize = 1 + JOB_BYTES / 8;
const PLANES: usize = 3;
const REGION_BYTES: u64 = 1 << 20;
const DST_OFFSET: u64 = REGION_BYTES / 2;

// lemnos_gsdma.h: the rotation and reflection bits the check uses (DRM's).
const ROTATE_0: u32 = 1 << 0;
const ROTATE_90: u32 = 1 << 1;
const ROTATE_270: u32 = 1 << 3;
const REFLECT_X: u32 = 1 << 4;

const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    (a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24)
}
const AR24: u32 = fourcc(b'A', b'R', b'2', b'4');
const NV12: u32 = fourcc(b'N', b'V', b'1', b'2');
const YUYV: u32 = fourcc(b'Y', b'U', b'Y', b'V');

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

// ---------------------------------------------------------------------------------------------
// The device: the control node, the heap, the block's name, and the device's attributes.
// ---------------------------------------------------------------------------------------------

struct Ctx {
    ctl: File,
    heap: File,
    block: u32,
    /// `/sys/bus/platform/devices/<device>`: the fake engine's attributes.
    sysdir: String,
    /// A real engine: nothing from the fake engine's files.
    board: bool,
}

/// `put_sys`: writes `text` to the fake engine's attribute `name`; false when that fails.
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

/// What the fake engine has run (its `ran` attribute): the chains it ran, the unreachable addresses
/// it saw, and the first words of the last chain.
#[derive(Default, Clone, Copy)]
struct Ran {
    jobs: u32,
    errors: u32,
    word0: [u32; 3],
}

/// `ran`: the fake engine's counts, or None when the attribute can't be read or parsed.
fn ran(c: &Ctx) -> Option<Ran> {
    let mut f = File::open(format!("{}/ran", c.sysdir)).ok()?;
    let mut text = [0u8; 128];
    let n = f.read(&mut text[..127]).ok()?;
    if n == 0 {
        return None;
    }
    let text = String::from_utf8_lossy(&text[..n]);
    let mut words = text.split_ascii_whitespace();
    let jobs = words.next()?.parse().ok()?;
    let errors = words.next()?.parse().ok()?;
    let mut word0 = [0u32; 3];
    for w in &mut word0 {
        let tok = words.next()?;
        *w = u32::from_str_radix(tok.strip_prefix("0x").unwrap_or(tok), 16).ok()?;
    }
    Some(Ran {
        jobs,
        errors,
        word0,
    })
}

// ---------------------------------------------------------------------------------------------
// A stream on the block: its ring, its image region, and the requests on it.
// ---------------------------------------------------------------------------------------------

/// The C's `struct stream`. The mappings go first, as the C unmaps before it closes, then the
/// descriptors: the stream, the region the image is in, and the pool.
struct Stream {
    ring: Mapping,
    /// The region, mapped: the source image at 0, the destination at `DST_OFFSET`.
    image: Mapping,
    fd: OwnedFd,
    region_fd: OwnedFd,
    _pool: OwnedFd,
    region_addr: u64,
    pool_addr: u64,
    reach: u64,
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

    /// The image's 32-bit word at byte `at`; 0 outside the region.
    fn image_u32(&self, at: u64) -> u32 {
        self.image
            .read_volatile::<4>(at as usize)
            .map_or(0, u32::from_ne_bytes)
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

/// `stream_open`: a stream on the block, with its pool and region made from the heap and the
/// region registered on it. The errno it failed with, when it fails.
fn stream_open(c: &Ctx) -> Result<Stream, i32> {
    // The C makes both dma-bufs before it looks at either, so the errno is the last failing one's.
    let pool = dma_heap::allocate(c.heap.as_fd(), u64::from(BUFFERS) * u64::from(BUFFER_BYTES))
        .map_err(|e| errno_of(&e));
    let region = dma_heap::allocate(c.heap.as_fd(), REGION_BYTES).map_err(|e| errno_of(&e));
    let region = region?;
    let pool = pool?;

    let mut arg = [0u8; 64];
    put32(&mut arg, 8, BUFFERS);
    put32(&mut arg, 12, BUFFER_BYTES);
    put32(&mut arg, 16, FRAME_SLOTS);
    put32(&mut arg, 20, JOB_SLOTS);
    put32(&mut arg, 24, JOB_BYTES as u32);
    put_i32(&mut arg, 28, pool.as_raw_fd());
    put_i32(&mut arg, 32, -1);
    put32(&mut arg, 0, c.block);
    let fd = guest::gsdma_create(c.ctl.as_fd(), &mut arg).map_err(|e| errno_of(&e))?;
    let ring_bytes = get64(&arg, 40) as usize;
    let pool_addr = get64(&arg, 48);
    let reach = get64(&arg, 56);
    // The image is mapped before the ring is, as the C does, so its errno is the one reported.
    let image =
        Mapping::new(region.as_fd(), REGION_BYTES as usize, 0, true).map_err(|e| errno_of(&e));
    let ring = Mapping::new(fd.as_fd(), ring_bytes, 0, true).map_err(|e| errno_of(&e));
    let image = image?;
    let ring = ring?;

    let control = shm_control(FRAME_SLOTS, BUFFERS);
    let mut reg = [0u8; 24];
    put_i32(&mut reg, 0, region.as_raw_fd());
    guest::ioctl_bytes(fd.as_fd(), REGISTER, &mut reg).map_err(|e| errno_of(&e))?;
    let region_addr = get64(&reg, 8);
    Ok(Stream {
        ring,
        image,
        fd,
        region_fd: region,
        _pool: pool,
        region_addr,
        pool_addr,
        reach,
        control,
        jobs_at: control + SHM_JOB_SLOTS,
        jobs_head: 0,
        frames_tail: 0,
    })
}

/// `struct lemnos_gsdma_job`, in its slot: 104 bytes.
#[derive(Clone, Copy, Default)]
struct Plane {
    src_addr: u64,
    dst_addr: u64,
    src_pitch: u32,
    dst_pitch: u32,
}

#[derive(Clone, Copy, Default)]
struct Job {
    identity: u64,
    buffer: u32,
    fourcc: u32,
    width: u32,
    height: u32,
    rotation: u32,
    planes: u32,
    plane: [Plane; PLANES],
}

impl Job {
    fn bytes(&self) -> [u8; JOB_BYTES] {
        let mut b = [0u8; JOB_BYTES];
        put64(&mut b, 0, self.identity);
        put32(&mut b, 8, self.buffer);
        put32(&mut b, 12, self.fourcc);
        put32(&mut b, 16, self.width);
        put32(&mut b, 20, self.height);
        put32(&mut b, 24, self.rotation);
        put32(&mut b, 28, self.planes);
        for (i, p) in self.plane.iter().enumerate() {
            let at = 32 + 24 * i;
            put64(&mut b, at, p.src_addr);
            put64(&mut b, at + 8, p.dst_addr);
            put32(&mut b, at + 16, p.src_pitch);
            put32(&mut b, at + 20, p.dst_pitch);
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
/// checked against its stamp. None while the frame is not out, and when its slot holds another
/// frame (the C's 0 and -1; the check takes only a 1).
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
    match guest::ioctl_bytes(s.fd.as_fd(), KICK, &mut [0u8; 0]) {
        Ok(()) => 0,
        Err(e) => errno_of(&e),
    }
}

/// The stream's status, as its request gives it (all 0xff when the request fails).
struct StreamStatus {
    state: u32,
    fault: i32,
    frames: u64,
    jobs: u64,
}

fn status_of(s: &Stream) -> StreamStatus {
    let mut b = [0xffu8; 40];
    let _ = guest::ioctl_bytes(s.fd.as_fd(), STREAM_STATUS, &mut b);
    StreamStatus {
        state: get32(&b, 0),
        fault: get32(&b, 4) as i32,
        frames: get64(&b, 8),
        jobs: get64(&b, 32),
    }
}

/// The block's status, counted since it was bound (all 0xff when the request fails).
struct BlockStatus {
    wedged: u32,
    finished: u64,
    refused: u64,
    timeouts: u64,
}

fn block_status(s: &Stream) -> BlockStatus {
    let mut b = [0xffu8; 32];
    let _ = guest::ioctl_bytes(s.fd.as_fd(), GSDMA_STATUS, &mut b);
    BlockStatus {
        wedged: get32(&b, 0),
        finished: get64(&b, 8),
        refused: get64(&b, 16),
        timeouts: get64(&b, 24),
    }
}

// ---------------------------------------------------------------------------------------------
// Jobs: the good ones, and the edits that make each refusal.
// ---------------------------------------------------------------------------------------------

/// An ARGB8888 copy of `w` x `h`, turned by `rotation`, from the region's start to its second half.
fn argb_job(s: &Stream, w: u32, h: u32, rotation: u32) -> Job {
    let swaps = (rotation & (ROTATE_90 | ROTATE_270)) != 0;
    let mut j = Job {
        identity: 0x1234,
        buffer: 0,
        fourcc: AR24,
        width: w,
        height: h,
        rotation,
        planes: 1,
        ..Default::default()
    };
    j.plane[0].src_addr = s.region_addr;
    j.plane[0].src_pitch = w * 4;
    j.plane[0].dst_addr = s.region_addr + DST_OFFSET;
    j.plane[0].dst_pitch = (if swaps { h } else { w }) * 4;
    j
}

/// One refusal's edit of a good job.
type Edit = fn(&Stream, &mut Job);

fn e_outside(s: &Stream, j: &mut Job) {
    j.plane[0].dst_addr = s.region_addr + REGION_BYTES;
}

fn e_past_end(s: &Stream, j: &mut Job) {
    // The last row ends one byte past the buffer: 3 rows of 32 bytes, then 33.
    j.plane[0].dst_addr = s.region_addr + REGION_BYTES - (3 * 32 + 31);
}

fn e_tall(_: &Stream, j: &mut Job) {
    // A pitch long enough to step past the buffer within the rows.
    j.plane[0].dst_pitch = 0xffff;
    j.height = 32;
}

fn e_pitch_short(_: &Stream, j: &mut Job) {
    j.plane[0].src_pitch = 31;
}

fn e_pitch_zero(_: &Stream, j: &mut Job) {
    j.plane[0].dst_pitch = 0;
}

fn e_pitch_wide(_: &Stream, j: &mut Job) {
    j.plane[0].dst_pitch = 0x10000;
}

fn e_turned_pitch(_: &Stream, j: &mut Job) {
    // Turned, the destination's row is the source's height: 4 * 4 bytes is fine, 8 * 4 isn't.
    j.rotation = ROTATE_90;
    j.width = 8;
    j.height = 8;
    j.plane[0].dst_pitch = 16;
}

fn e_wrap(_: &Stream, j: &mut Job) {
    j.plane[0].src_addr = u64::MAX - 40;
}

fn e_overlap(s: &Stream, j: &mut Job) {
    j.plane[0].dst_addr = s.region_addr + 64;
}

fn e_pool(s: &Stream, j: &mut Job) {
    j.plane[0].dst_addr = s.pool_addr;
}

fn e_unregistered(s: &Stream, j: &mut Job) {
    // Memory nobody registered: its address is a guess, and every guess is outside.
    j.plane[0].dst_addr = s.region_addr + 2 * REGION_BYTES;
}

fn e_two_turns(_: &Stream, j: &mut Job) {
    j.rotation = ROTATE_0 | ROTATE_90;
}

fn e_no_turn(_: &Stream, j: &mut Job) {
    j.rotation = REFLECT_X;
}

fn e_unknown_bit(_: &Stream, j: &mut Job) {
    j.rotation |= 1 << 6;
}

fn e_format(_: &Stream, j: &mut Job) {
    j.fourcc = YUYV;
}

fn e_planes(_: &Stream, j: &mut Job) {
    j.planes = 2;
}

fn e_spare_plane(_: &Stream, j: &mut Job) {
    j.plane[1].src_pitch = 1;
}

fn e_width0(_: &Stream, j: &mut Job) {
    j.width = 0;
}

fn e_width_big(_: &Stream, j: &mut Job) {
    j.width = 0x10000;
}

fn e_buffer(_: &Stream, j: &mut Job) {
    j.buffer = BUFFERS;
}

fn e_nv12_overlap(s: &Stream, j: &mut Job) {
    // NV12 8x4: luma 32 bytes, chroma 2 rows of 8. Chroma's destination lands on luma's.
    j.fourcc = NV12;
    j.planes = 2;
    j.plane[0].src_pitch = 8;
    j.plane[0].dst_pitch = 8;
    j.plane[1].src_addr = s.region_addr + 4096;
    j.plane[1].dst_addr = s.region_addr + DST_OFFSET + 8;
    j.plane[1].src_pitch = 8;
    j.plane[1].dst_pitch = 8;
}

/// `px`: the pixel at (`x`, `y`) of the source image.
fn px(x: u32, y: u32) -> u32 {
    0xa500_0000 | (y << 8) | x
}

/// A job that must be refused with `err`, ending its stream, on a stream of its own.
fn refused(c: &Ctx, what: &str, err: i32, edit: Option<Edit>) {
    let mut st = match stream_open(c) {
        Ok(st) => st,
        Err(e) => {
            fail(&format!("FAIL {what}: stream ({})", strerror(e)));
            return;
        }
    };
    let mut job = argb_job(&st, 8, 4, ROTATE_0);
    if let Some(edit) = edit {
        edit(&st, &mut job);
    }
    let before = if c.board {
        0
    } else {
        ran(c).unwrap_or_default().jobs
    };
    let finished = block_status(&st).finished;
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
    // Nothing reached the engine.
    check!(
        block_status(&st).finished == finished,
        "block_status(&st).finished == finished"
    );
    if !c.board {
        let after = ran(c).unwrap_or_default().jobs;
        check!(after == before, "after[0] == before[0]");
    }
}

// ---------------------------------------------------------------------------------------------
// The good copies, and the runs.
// ---------------------------------------------------------------------------------------------

/// A good ARGB job, turned by `rotation`, checked pixel by pixel.
fn good_argb(c: &Ctx, s: &mut Stream, rotation: u32, identity: u64) {
    const W: u32 = 8;
    const H: u32 = 4;
    let mut j = argb_job(s, W, H, rotation);
    for y in 0..H {
        for x in 0..W {
            let _ = s
                .image
                .write_bytes(4 * (y * W + x) as usize, &px(x, y).to_ne_bytes());
        }
    }
    let _ = s
        .image
        .write_bytes(DST_OFFSET as usize, &[0u8; 4 * (W * H) as usize]);
    j.identity = identity;
    post(s, &j);
    check!(kick(s) == 0, "kick(st) == 0");
    let mut got_id = 0u64;
    let bytes = wait_frame(s, &mut got_id, 2000);
    if bytes != i64::from(BUFFER_BYTES) && c.board {
        fail(&format!("FAIL frame of {bytes} bytes for job {identity}"));
    } else if bytes != i64::from(BUFFER_BYTES) {
        let b = block_status(s);
        let st = status_of(s);
        let r = ran(c).unwrap_or_default();
        fail(&format!(
            "FAIL frame of {bytes} bytes for job {identity}: finished {} refused {} \
             timeouts {}; stream state {} fault {} jobs {} frames {}; \
             engine ran {} ({} errors) {:x}",
            b.finished,
            b.refused,
            b.timeouts,
            st.state,
            st.fault,
            st.jobs,
            st.frames,
            r.jobs,
            r.errors,
            r.word0[0]
        ));
    }
    check!(got_id == identity, "got_id == identity");
    let mut bad = 0u32;
    for y in 0..H {
        for x in 0..W {
            let at = if (rotation & ROTATE_90) != 0 {
                DST_OFFSET + 4 * u64::from(x * H + (H - 1 - y))
            } else {
                DST_OFFSET + 4 * u64::from(y * W + x)
            };
            bad += u32::from(s.image_u32(at) != px(x, y));
        }
    }
    if bad > 0 {
        fail(&format!("FAIL rotation {rotation:#x}: {bad} pixels wrong"));
    }
}

/// The good jobs, the refusals, the hung job, and the stream's lifetime, on a real or a fake engine.
fn run_all(c: &Ctx) {
    // Refusals, each ending its own stream.
    refused(
        c,
        "a destination past every registered dma-buf",
        libc::EFAULT,
        Some(e_outside),
    );
    refused(
        c,
        "a destination whose last row ends past the buffer",
        libc::EFAULT,
        Some(e_past_end),
    );
    refused(
        c,
        "rows that step past the buffer",
        libc::EFAULT,
        Some(e_tall),
    );
    refused(
        c,
        "a destination in memory never registered",
        libc::EFAULT,
        Some(e_unregistered),
    );
    refused(
        c,
        "a pitch shorter than a row",
        libc::EINVAL,
        Some(e_pitch_short),
    );
    refused(c, "a pitch of 0", libc::EINVAL, Some(e_pitch_zero));
    refused(
        c,
        "a pitch the descriptor can't hold",
        libc::EINVAL,
        Some(e_pitch_wide),
    );
    refused(
        c,
        "a turned destination's pitch shorter than its row",
        libc::EINVAL,
        Some(e_turned_pitch),
    );
    refused(c, "a source whose end wraps", libc::EOVERFLOW, Some(e_wrap));
    refused(
        c,
        "a destination over the source",
        libc::ENOTUNIQ,
        Some(e_overlap),
    );
    refused(
        c,
        "a second plane's destination over the first's",
        libc::ENOTUNIQ,
        Some(e_nv12_overlap),
    );
    refused(c, "a destination in the pool", libc::EACCES, Some(e_pool));
    refused(c, "two turns", libc::EINVAL, Some(e_two_turns));
    refused(c, "no turn", libc::EINVAL, Some(e_no_turn));
    refused(
        c,
        "an unknown rotation bit",
        libc::EINVAL,
        Some(e_unknown_bit),
    );
    refused(c, "an unknown format", libc::EINVAL, Some(e_format));
    refused(
        c,
        "a plane count not the format's",
        libc::EINVAL,
        Some(e_planes),
    );
    refused(
        c,
        "a non-zero plane the format hasn't",
        libc::EINVAL,
        Some(e_spare_plane),
    );
    refused(c, "a width of 0", libc::EINVAL, Some(e_width0));
    refused(
        c,
        "a width the descriptor can't hold",
        libc::EINVAL,
        Some(e_width_big),
    );
    refused(
        c,
        "a buffer past the stream's",
        libc::EINVAL,
        Some(e_buffer),
    );

    // Good jobs.
    let mut st = match stream_open(c) {
        Ok(st) => st,
        Err(e) => {
            fail(&format!("FAIL stream for the good jobs: {}", strerror(e)));
            return;
        }
    };
    check!(st.reach == 0x8000_0000, "st.reach == 0x80000000ull");
    if c.board {
        say(&format!(
            "reach {:#x}, image at {:#x}",
            st.reach, st.region_addr
        ));
    }
    let ran0 = if c.board { None } else { ran(c) };
    check!(
        c.board || ran0.is_some(),
        "board || !ran(&jobs0, &errors, word0)"
    );
    let jobs0 = ran0.unwrap_or_default().jobs;
    good_argb(c, &mut st, ROTATE_0, 1);
    good_argb(c, &mut st, ROTATE_90, 2);
    if !c.board {
        let got = ran(c);
        check!(got.is_some(), "!ran(&jobs, &errors, word0)");
        let r = got.unwrap_or_default();
        check!(r.jobs == jobs0.wrapping_add(2), "jobs == jobs0 + 2");
        check!(r.errors == 0, "errors == 0");
        // The last was one plane, a quarter turn, 32-bit elements, interrupt at its end.
        check!(
            r.word0[0] == (1 | (3 << 8) | (1 << 30)),
            "word0[0] == (1u | 3u << 8 | 1u << 30)"
        );
    }

    // NV12, mirrored across: two descriptors, the interrupt on the second.
    let mut j = argb_job(&st, 8, 4, ROTATE_0 | REFLECT_X);
    e_nv12_overlap(&st, &mut j);
    j.plane[1].dst_addr = st.region_addr + DST_OFFSET + 4096;
    j.identity = 3;
    post(&mut st, &j);
    check!(kick(&st) == 0, "kick(&st) == 0");
    let mut id = 0u64;
    check!(
        wait_frame(&mut st, &mut id, 2000) == i64::from(BUFFER_BYTES),
        "wait_frame(&st, &id, 2000) == BUFFER_BYTES"
    );
    check!(id == 3, "id == 3");
    check!(
        block_status(&st).finished >= 3,
        "block_status(&st).finished >= 3"
    );
    if !c.board {
        let got = ran(c);
        check!(got.is_some(), "!ran(&jobs, &errors, word0)");
        let r = got.unwrap_or_default();
        check!(r.word0[0] == (1 << 2), "word0[0] == (1u << 2)");
        check!(
            r.word0[1] == ((1 << 2) | (1 << 8) | (1 << 30)),
            "word0[1] == (1u << 2 | 1u << 8 | 1u << 30)"
        );

        // A job that never finishes: stopped, published empty, and the stream goes on.
        check!(put_sys(c, "hang", "20"), "!put_sys(\"hang\", \"20\")");
        let mut j = argb_job(&st, 8, 4, ROTATE_0);
        j.identity = 4;
        post(&mut st, &j);
        check!(kick(&st) == 0, "kick(&st) == 0");
        check!(
            wait_frame(&mut st, &mut id, 3000) == 0,
            "wait_frame(&st, &id, 3000) == 0"
        );
        check!(id == 4, "id == 4");
        check!(
            block_status(&st).timeouts == 1,
            "block_status(&st).timeouts == 1"
        );
        check!(
            block_status(&st).wedged == 0,
            "block_status(&st).wedged == 0"
        );
        check!(
            status_of(&st).state == RING_OPEN,
            "status_of(&st).state == LEMNOS_RING_STATE_OPEN"
        );
        check!(put_sys(c, "hang", "0"), "!put_sys(\"hang\", \"0\")");
        good_argb(c, &mut st, ROTATE_0, 5);
        check!(
            status_of(&st).state == RING_OPEN,
            "status_of(&st).state == LEMNOS_RING_STATE_OPEN"
        );
    }

    // One stream at a time.
    check!(
        matches!(stream_open(c), Err(libc::EBUSY)),
        "stream_open(&second) && errno == EBUSY"
    );
    drop(st);
    // Closed: the block takes a stream again.
    let reopened = stream_open(c);
    check!(reopened.is_ok(), "!stream_open(&st)");
    let Ok(mut st) = reopened else {
        return;
    };
    good_argb(c, &mut st, ROTATE_0, 6);
    // The engine never read a freed page: the fake counts a stale descriptor as an error.
    let got = ran(c);
    check_at(
        got.is_some_and(|r| r.errors == 0),
        "no stale descriptor was read",
        line!(),
    );
}

/// With the reach lowered below every buffer: every image is past it.
fn run_reach(c: &Ctx) {
    let st = match stream_open(c) {
        Ok(st) => st,
        Err(e) => {
            fail(&format!("FAIL stream: {}", strerror(e)));
            return;
        }
    };
    check!(st.reach < st.region_addr, "st.reach < st.region_addr");
    drop(st);
    refused(c, "an image past the engine's reach", libc::ENXIO, None);
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

/// An engine whose progress never stops: wedged, and its job's memory stays held.
fn run_wedge(c: &Ctx) {
    let mut st = match stream_open(c) {
        Ok(st) => st,
        Err(e) => {
            fail(&format!("FAIL stream: {}", strerror(e)));
            return;
        }
    };
    check!(
        put_sys(c, "hang", "4294967295"),
        "!put_sys(\"hang\", \"4294967295\")"
    );
    let j = argb_job(&st, 8, 4, ROTATE_0);
    post(&mut st, &j);
    check!(kick(&st) == 0, "kick(&st) == 0");
    let mut t = 0;
    while t < 200 && status_of(&st).state == RING_OPEN {
        thread::sleep(Duration::from_micros(5000));
        t += 1;
    }
    {
        let b = block_status(&st);
        let s = status_of(&st);
        let r = ran(c).unwrap_or_default();
        if s.state != RING_FAULT || s.fault != libc::EIO || b.wedged != 1 {
            fail(&format!(
                "FAIL not wedged: state {} fault {} jobs {} frames {}; wedged {} \
                 finished {} timeouts {}; engine ran {}",
                s.state, s.fault, s.jobs, s.frames, b.wedged, b.finished, b.timeouts, r.jobs
            ));
        }
    }
    // The stream is gone, its memory isn't: the kernel still holds the region.
    let region = st.region_fd.try_clone().ok();
    drop(st);
    check!(
        region
            .as_ref()
            .and_then(|r| fdinfo_count(r.as_raw_fd()))
            .is_some_and(|n| n > 1),
        "p && strtol(p + 6, NULL, 10) > 1"
    );
    drop(region);
    check!(
        matches!(stream_open(c), Err(libc::EIO)),
        "stream_open(&st) && errno == EIO"
    );
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
        let name = args.first().map_or("lemnos_gsdma_check", String::as_str);
        let _ = writeln!(
            io::stderr(),
            "usage: {name} /dev/lemnos <device> <heap> <run|board|reach|wedge>"
        );
        return ExitCode::from(2);
    }
    // The fake engine's tick runs on CPU 0; the driver's progress reads share its vCPU.
    if let Err(e) = guest::set_cpu_affinity(Some(0)) {
        let _ = writeln!(
            io::stderr(),
            "sched_setaffinity: {}",
            strerror(errno_of(&e))
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
        "run" | "board" => run_all(&c),
        "reach" => run_reach(&c),
        "wedge" => run_wedge(&c),
        _ => return ExitCode::from(2),
    }
    say(&format!("lemnos_gsdma_check {mode}: {} failed", failures()));
    if failures() > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
