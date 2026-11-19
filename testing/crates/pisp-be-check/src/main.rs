//! The PiSP back end's job checks, against the fake back end of the test harness (the
//! `lemnos-test-pisp` block, module parameter `test_pb_fake_regs`).
//!
//! Every field that names memory, every size and every index is tried out of bounds, wrapped and
//! disagreeing with its format, on a job that is otherwise good, and the exact errno is required.
//! Accepted jobs have their registers read back: what the block is given has to be what was
//! checked, even with the config slot rewritten while the job is in flight, and a config flipped
//! from valid to invalid while the check runs. Interrupt accounting: counters beyond the jobs held
//! wedge the block rather than finishing anything. Teardown: closing or unbinding with a job on the
//! block waits for it; a block that never finishes is wedged and its buffers are never released.
//!
//! Usage: `lemnos-pisp-be-check /dev/lemnos <device> <bind> <unbind> <heap> <refuse|lifetime|wedge>`
//! (init runs it as `/lemnos_pisp_be_check`). Port of `kernel/test/lemnos_pisp_be_check.c`
//! (CZ-GUEST-3).
//!
//! The uAPI structures are laid out in bytes, sized as the C structures are: `lemnos.h`'s
//! `lemnos_stream_create` (56 bytes, inside the 80-byte `lemnos_pisp_be_create`),
//! `lemnos_stream_register` (24), `lemnos_stream_unregister` (8), `lemnos_stream_status` (40),
//! `lemnos_info` (32), and `lemnos_pisp_be.h`'s `lemnos_pisp_be_status` (24) and
//! `lemnos_pisp_be_job` (120). The configuration is `struct pisp_be_tiles_config` from the kernel's
//! `pisp_be_config.h` (16720 bytes). Its offsets, and the enum values used, were counted with the C
//! compiler against the kernel tree's headers. The fixture is `fixture.rs`, which `drivers/pisp`
//! generates from the configuration library and checks.

use std::cell::Cell;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};
use std::thread;
use std::time::{Duration, Instant};

mod fixture;

use fixture::{FIXTURE, OUTPUT_CHROMA, OUTPUT_LUMA};
use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::{dma_heap, guest, ioc, ior, iow, iowr, module};

/// The fake block's sysfs directory: `poke`, `irq`, `regs`, `page_refs`.
const SYS: &str = "/sys/bus/platform/devices/lemnos-test-pisp";

// lemnos.h: the stream's requests on the stream's file, and the info request on the control node.
// `KICK` takes no argument, so it is a zero-length request, as lend-check issues it.
const KICK: u32 = ioc(0, b'L', 0x81, 0);
const STREAM_REGISTER: u32 = iowr::<[u8; 24]>(b'L', 0x82);
const STREAM_UNREGISTER: u32 = iow::<[u8; 8]>(b'L', 0x83);
const STREAM_STATUS: u32 = ior::<[u8; 40]>(b'L', 0x84);
const IOC_INFO: u32 = iowr::<[u8; 32]>(b'L', 0xd0);
// lemnos_pisp_be.h: the back end's status, on the stream's file.
const PISP_STATUS: u32 = ior::<[u8; 24]>(b'L', 0xb1);
const _: () = assert!(
    KICK == 0x0000_4c81
        && STREAM_REGISTER == 0xc018_4c82
        && STREAM_UNREGISTER == 0x4008_4c83
        && STREAM_STATUS == 0x8028_4c84
        && IOC_INFO == 0xc020_4cd0
        && PISP_STATUS == 0x8018_4cb1
);

// The stream the check makes: a pool of BUFFERS buffers, FRAME_SLOTS frame slots, JOB_SLOTS job
// slots of a job of JOB_BYTES bytes.
const BUFFERS: u32 = 4;
const BUFFER_BYTES: u64 = 256 * 1024;
const REGION_BYTES: u64 = 4 * 1024 * 1024;
const FRAME_SLOTS: usize = 8;
const JOB_SLOTS: usize = 4;
const JOB_BYTES: usize = 120;
/// One length word, then the job's words: 16 words a slot.
const JOB_WORDS: usize = 16;
const W: u16 = 64;
const H: u16 = 32;

// kernel/rs/stream_ring.rs (layout 2): the word numbers of the shared-memory ring, as word indices
// into the ring (the module's ring is read as 64-bit words). The frame index holds, for each frame
// slot, the lane slot the frame was published in (from bit 16). A lane slot is SHM_SLOT_WORDS
// words: the stamp, the time, the meta, then the 48-byte entry from SHM_ENTRY on (the buffer in the
// pair word, the identity at SHM_ENTRY + 3, the end at SHM_ENTRY + 5). The job block follows the
// pool page: its control block, then the job slots.
const SHM_W_HEAD: usize = 8;
const SHM_INDEX: usize = 17;
const SHM_SLOT_WORDS: usize = 10;
const SHM_ENTRY: usize = 4;
const SHM_TICKETS: usize = 8;
const SHM_READERS: usize = 64;
const SHM_JOB_HEAD: usize = 2;
const SHM_JOB_SLOTS: usize = 4;

/// `shm_page_at`: the pool page's first word, after the frame ring.
const fn shm_page_at(frame_slots: usize) -> usize {
    SHM_INDEX + frame_slots + frame_slots * SHM_SLOT_WORDS
}

/// `shm_control`: the control block's first word, after the pool page's tickets and cut-off words.
const fn shm_control(frame_slots: usize, buffers: usize) -> usize {
    shm_page_at(frame_slots) + SHM_TICKETS + buffers + SHM_READERS
}

/// The control block: the job head is at `CONTROL + SHM_JOB_HEAD`.
const CONTROL: usize = shm_control(FRAME_SLOTS, BUFFERS as usize);
/// The first job slot: each is one length word then the job's words.
const JOBS_AT: usize = CONTROL + SHM_JOB_SLOTS;

/// What `shm_frame_read` found for one frame sequence number.
enum FrameRead {
    /// The module has not published it yet (the frame head has not passed it).
    NotYet,
    /// Its slot holds another frame, or its stamp moved while it was read: the consumer was too
    /// slow.
    Reused,
    /// The frame, read once and checked against its stamp.
    Frame(ShmFrame),
}

/// How long a job's frame may take to appear once its completion is posted, on the monotonic clock.
/// This is the bound the check has always had (2 s); it is a clock bound now, not a count of sleeps.
const FRAME_WAIT: Duration = Duration::from_secs(2);

/// What a frame wait got: the frame's buffer and identity (buffer -1 when none came in time), the
/// frames of other jobs passed over first, the time the wait took, and the ring's head and tail then.
#[derive(Debug, Clone, Copy)]
struct Got {
    buffer: i64,
    identity: u64,
    stale: u32,
    elapsed: Duration,
    head: u64,
    tail: u64,
}

/// The module's own view of the block, from its test-only parameters (`test_pb_*` in pisp_be.rs): the
/// watchdog's runs and recoveries, and the job state the last interrupt or watchdog left.
fn module_view() -> String {
    let read = |name: &str| -> u64 {
        fs::read_to_string(format!("/sys/module/lemnos/parameters/{name}"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(u64::MAX)
    };
    let job = read("test_pb_job");
    let bit = |n: u32| (job >> n) & 1;
    let byte = |n: u32| (job >> n) & 0xff;
    format!(
        "watchdog runs {} recoveries {} last recovery at {} ns (monotonic); job: queued {} running {} ready {} busy {} wedged {}, stream started {} done {}, block started {} done {}",
        read("test_pb_wd_runs"),
        read("test_pb_wd_recovered"),
        read("test_pb_wd_recovered_ns"),
        bit(0),
        bit(1),
        bit(2),
        bit(3),
        bit(4),
        byte(8),
        byte(16),
        byte(32),
        byte(24),
    )
}

/// The fields of a published frame that the check uses.
struct ShmFrame {
    buffer: u32,
    identity: u64,
    timestamp_ns: u64,
    end_ns: u64,
}

/// Frame `seq` through the index, as a consumer reads the ring (`kernel/rs/stream_ring.rs`). The
/// time, pair, identity and end words are read between two reads of the stamp.
fn shm_frame_read(ring: &[AtomicU64], frame_slots: usize, seq: u64) -> FrameRead {
    if ring[SHM_W_HEAD].load(Ordering::Acquire) <= seq {
        return FrameRead::NotYet;
    }
    let index = SHM_INDEX + (seq & (frame_slots as u64 - 1)) as usize;
    let slot = (ring[index].load(Ordering::Acquire) >> 16) as usize;
    let at = SHM_INDEX + frame_slots + slot * SHM_SLOT_WORDS;
    let stamp = ring[at].load(Ordering::Acquire);
    if stamp != (seq + 1) << 1 {
        return FrameRead::Reused;
    }
    let timestamp_ns = ring[at + 1].load(Ordering::Acquire);
    let pair = ring[at + SHM_ENTRY + 1].load(Ordering::Acquire);
    let identity = ring[at + SHM_ENTRY + 3].load(Ordering::Acquire);
    let end_ns = ring[at + SHM_ENTRY + 5].load(Ordering::Acquire);
    if ring[at].load(Ordering::Acquire) != stamp {
        return FrameRead::Reused;
    }
    FrameRead::Frame(ShmFrame {
        buffer: pair as u32,
        identity,
        timestamp_ns,
        end_ns,
    })
}

// lemnos.h: the state values.
const STATE_OPEN: u32 = 0;
const STATE_CLOSED: u32 = 1;
const STATE_FAULT: u32 = 2;

// The fake block's registers.
const BE_CONTROL: u32 = 0x04;
const BE_TILE_LO: u32 = 0x08;
const BE_STATUS: u32 = 0x10;
const BE_BATCH: u32 = 0x14;
const BE_INT_STATUS: u32 = 0x1c;
const BE_CONFIG: u32 = 0x40;
/// The fake block's control word: the first word past the registers (`LPB_FAKE_CTL` in
/// kernel/rs/pisp_be.rs, which is 0x40 + sizeof(pisp_be_config)).
const BE_FAKE_CTL: u32 = 0x198c;
/// The fake block's control values (kernel/rs/pisp_be.rs). DONE: the block finishes each job it is
/// given and raises its interrupt, as the real block does. HOLD: the job stays on the block until a
/// check completes it by hand. LOSE: the job is finished but its interrupt is not raised.
const FAKE_DONE: u32 = 0;
const FAKE_HOLD: u32 = 1;
const FAKE_LOSE: u32 = 2;

// lemnos.h: LEMNOS_INFO_MMIO_MAPPABLE.
const INFO_MMIO_MAPPABLE: u32 = 1;

// `struct pisp_be_tiles_config` (16720 bytes): `config` at 0 (6476 bytes), `tiles[64]` at 6476
// (160 bytes each), `num_tiles` at 16716. Within `config`, the offsets of the fields used.
const CONFIG_BYTES: usize = 16720;
const CFG_BAYER_ENABLES: usize = 112;
const CFG_RGB_ENABLES: usize = 116;
const CFG_BAYER_ORDER: usize = 120;
const CFG_INPUT: usize = 124;
const CFG_TDN_INPUT: usize = 156;
const CFG_TDN: usize = 176;
const CFG_TDN_OUTPUT: usize = 192;
const CFG_STITCH_OUTPUT: usize = 236;
const CFG_STITCH_INPUT: usize = 252;
/// `output_format[0]`: 28 bytes a plane's format (`image` 16 bytes, then `transform`).
const CFG_OUT_FORMAT: usize = 6324;
const OUT_FORMAT_BYTES: usize = 28;
const OUT_TRANSFORM: usize = 16;
const CFG_AXI: usize = 6388;
const TILES: usize = 6476;
const TILE_BYTES: usize = 160;
const CFG_NUM_TILES: usize = 16716;
/// Within a `pisp_image_format_config` (16 bytes): width, height, format, stride, stride2.
const F_W: usize = 0;
const F_H: usize = 2;
const F_FMT: usize = 4;
const F_STRIDE: usize = 8;
const F_STRIDE2: usize = 12;
/// Within a `pisp_tile`.
const T_IN_ADDR: usize = 4;
const T_IN_ADDR2: usize = 8;
const T_IN_W: usize = 16;
const T_IN_H: usize = 18;
const T_IN_X: usize = 12;
const T_IN_Y: usize = 14;
const T_TDN_IN_ADDR: usize = 20;
const T_TDN_OUT_ADDR: usize = 24;
const T_STITCH_IN_ADDR: usize = 28;
const T_STITCH_OUT_ADDR: usize = 32;
const T_OUT_Y: usize = 128;
const T_OUT_W: usize = 132;
const T_OUT_H: usize = 136;
const T_OUT_ADDR: usize = 140;
const T_OUT_ADDR2: usize = 148;

// The enum values the config uses.
const BAYER_INPUT: u32 = 0x0000_0001;
const BAYER_DECOMPRESS: u32 = 0x0000_0002;
const BAYER_TDN_INPUT: u32 = 0x0000_0010;
const BAYER_TDN_DECOMPRESS: u32 = 0x0000_0020;
const BAYER_TDN: u32 = 0x0000_0040;
const BAYER_TDN_COMPRESS: u32 = 0x0000_0080;
const BAYER_TDN_OUTPUT: u32 = 0x0000_0100;
const BAYER_STITCH_INPUT: u32 = 0x0000_0800;
const BAYER_STITCH_DECOMPRESS: u32 = 0x0000_1000;
const BAYER_STITCH: u32 = 0x0000_2000;
const BAYER_STITCH_COMPRESS: u32 = 0x0000_4000;
const BAYER_STITCH_OUTPUT: u32 = 0x0000_8000;
const RGB_INPUT: u32 = 0x0000_0001;
const RGB_OUTPUT0: u32 = 0x0004_0000;
const RGB_OUTPUT1: u32 = 0x0008_0000;
const RGB_HOG: u32 = 0x0020_0000;
const FMT_THREE: u32 = 0x4000_0000;
const FMT_BPS8: u32 = 0x0000_0000;
const FMT_BPS16: u32 = 0x0000_0003;
const FMT_PLANAR: u32 = 0x0000_0020;
const FMT_SAMPLING420: u32 = 0x0000_0200;
const FMT_COMPRESSION1: u32 = 0x0100_0000;
const FMT_HOG_SIGNED: u32 = 0x0400_0000;
const FMT_INTEGRAL: u32 = 0x1000_0000;
const FMT_WALLPAPER: u32 = 0x2000_0000;
const TRANSFORM_HFLIP: u8 = 0x1;
const TRANSFORM_VFLIP: u8 = 0x2;

/// The job's device addresses: 13 of them, in the back end's register order.
const ADDRS: usize = 13;
type Addrs = [u64; ADDRS];

/// The address slot of output `n`, plane `plane`.
const fn out_addr(n: usize, plane: usize) -> usize {
    7 + 3 * n + plane
}

/// The config's `output_format[n]`.
const fn out_format(n: usize) -> usize {
    CFG_OUT_FORMAT + n * OUT_FORMAT_BYTES
}

/// Config byte offset of tile `i`.
const fn tile(i: usize) -> usize {
    TILES + i * TILE_BYTES
}

const EINVAL: i32 = libc::EINVAL;
const EFAULT: i32 = libc::EFAULT;
const EOVERFLOW: i32 = libc::EOVERFLOW;
const EACCES: i32 = libc::EACCES;
const EOPNOTSUPP: i32 = libc::EOPNOTSUPP;
const EBUSY: i32 = libc::EBUSY;
const EMSGSIZE: i32 = libc::EMSGSIZE;
const ERANGE: i32 = libc::ERANGE;
const ENODEV: i32 = libc::ENODEV;
const ETIMEDOUT: i32 = libc::ETIMEDOUT;
const EIO: i32 = libc::EIO;
const EMFILE: i32 = libc::EMFILE;

/// The failures counted so far. Atomic because the threads that complete jobs count theirs too.
static FAILURES: AtomicU32 = AtomicU32::new(0);

/// Prints one line, unbuffered as the C check's `printf` is.
fn say(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// A failed check: prints `FAIL` and the text, and counts it.
fn fail(what: &str) {
    say(&format!("FAIL {what}"));
    FAILURES.fetch_add(1, Ordering::Relaxed);
}

/// `CHECK`: a failed check prints its expression, which is the text the C check gives it.
fn check(ok: bool, what: &str) {
    if !ok {
        fail(what);
    }
}

fn failures() -> u32 {
    FAILURES.load(Ordering::Relaxed)
}

/// `strerror`: the text of an errno, as the C check's musl `strerror` gives it.
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

/// `write_node`: writes `bytes` to the file at `path`. True when it opened and took every byte.
fn write_node(path: &str, bytes: &[u8]) -> bool {
    OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(path)
        .and_then(|mut f| f.write(bytes))
        .is_ok_and(|n| n == bytes.len())
}

/// Writes `bytes` to an open file: true when it took every byte.
fn write_len(f: &mut File, bytes: &[u8]) -> bool {
    f.write(bytes).is_ok_and(|n| n == bytes.len())
}

/// Writes `off value` to the fake block's `poke` file.
fn poke(off: u32, value: u32) {
    let text = format!("{off:x} {value:x}");
    if !write_node(&format!("{SYS}/poke"), text.as_bytes()) {
        fail(&format!("poke {off:x}"));
    }
}

/// Sets the fake block's control word (see `BE_FAKE_CTL`).
fn fake_mode(mode: u32) {
    poke(BE_FAKE_CTL, mode);
}

/// Raises the fake block's interrupt.
fn raise_irq() {
    if !write_node(&format!("{SYS}/irq"), b"1") {
        fail("irq");
    }
}

/// Reads the fake block's registers into `buf`, from its `regs` file. When the file does not open,
/// nothing is read and nothing is reported.
fn read_regs(buf: &mut [u8]) {
    let Ok(f) = File::open(format!("{SYS}/regs")) else {
        return;
    };
    let n = buf.len();
    let mut at = 0;
    while at < n {
        match f.read_at(&mut buf[at..], at as u64) {
            Ok(got) if got > 0 => at += got,
            _ => {
                fail("regs");
                break;
            }
        }
    }
}

/// The address of the tiles the block was last given, from its registers.
fn last_tiles() -> u64 {
    let mut regs = [0u8; 16];
    read_regs(&mut regs);
    u64::from(get32(&regs, BE_TILE_LO as usize))
        | (u64::from(get32(&regs, BE_TILE_LO as usize + 4)) << 32)
}

/// The page's reference count, as the module's `page_refs` file gives it: 0 once the kernel has
/// freed it, negative when the file refused the address.
fn page_refs(addr: u64) -> i64 {
    let path = format!("{SYS}/page_refs");
    let text = format!("{addr:x}");
    let Ok(mut f) = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(&path)
    else {
        return -2;
    };
    if f.write(text.as_bytes()).ok() != Some(text.len()) {
        return -2;
    }
    drop(f);
    let Ok(bytes) = fs::read(&path) else {
        return -2;
    };
    let bytes = &bytes[..bytes.len().min(31)];
    if bytes.is_empty() {
        return -2;
    }
    parse_long(bytes)
}

/// The reference count of a dma-buf, from its fdinfo: above 1 means the kernel still holds it.
/// -1 when the descriptor's fdinfo cannot be read.
fn dmabuf_count(fd: i32) -> i64 {
    let Ok(bytes) = fs::read(format!("/proc/self/fdinfo/{fd}")) else {
        return -1;
    };
    let text = &bytes[..bytes.len().min(255)];
    match find(text, b"count:") {
        Some(p) => parse_long(&text[p + 6..]),
        None => -1,
    }
}

/// The 16-byte format of an image, as a config writes it.
#[derive(Clone, Copy)]
struct Image {
    w: u16,
    h: u16,
    fmt: u32,
    stride: i32,
    stride2: i32,
}

/// `struct pisp_be_tiles_config`, as its bytes.
struct Config {
    b: [u8; CONFIG_BYTES],
}

impl Config {
    fn zeroed() -> Self {
        Self {
            b: [0; CONFIG_BYTES],
        }
    }

    fn get32(&self, at: usize) -> u32 {
        get32(&self.b, at)
    }

    fn set8(&mut self, at: usize, v: u8) {
        self.b[at] = v;
    }

    fn set16(&mut self, at: usize, v: u16) {
        put16(&mut self.b, at, v);
    }

    fn set32(&mut self, at: usize, v: u32) {
        put32(&mut self.b, at, v);
    }

    fn set_i32(&mut self, at: usize, v: i32) {
        put_i32(&mut self.b, at, v);
    }

    fn or32(&mut self, at: usize, v: u32) {
        let x = self.get32(at) | v;
        self.set32(at, x);
    }

    fn and32(&mut self, at: usize, v: u32) {
        let x = self.get32(at) & v;
        self.set32(at, x);
    }

    fn set_image(&mut self, at: usize, img: Image) {
        self.set16(at + F_W, img.w);
        self.set16(at + F_H, img.h);
        self.set32(at + F_FMT, img.fmt);
        self.set_i32(at + F_STRIDE, img.stride);
        self.set_i32(at + F_STRIDE2, img.stride2);
    }

    /// Copies tile `from` over tile `to`.
    fn copy_tile(&mut self, from: usize, to: usize) {
        self.b
            .copy_within(tile(from)..tile(from) + TILE_BYTES, tile(to));
    }
}

fn rgb_image() -> Image {
    Image {
        w: W,
        h: H,
        fmt: FMT_THREE | FMT_BPS8,
        stride: 192,
        stride2: 0,
    }
}

fn yuv_image() -> Image {
    Image {
        w: W,
        h: H,
        fmt: FMT_THREE | FMT_BPS8 | FMT_PLANAR | FMT_SAMPLING420,
        stride: 64,
        stride2: 32,
    }
}

/// The check's descriptors: the control node, the CMA heap, and the block under test.
struct Env {
    ctl: Option<OwnedFd>,
    heap: OwnedFd,
    block: u32,
}

impl Env {
    fn ctl(&self) -> BorrowedFd<'_> {
        self.ctl
            .as_ref()
            .map(AsFd::as_fd)
            .expect("the control node is open")
    }
}

/// The block's number for `device`, or 0 when no block has that name.
fn find_block(ctl: BorrowedFd<'_>, device: &str) -> u32 {
    module::find_block(ctl, device).ok().flatten().unwrap_or(0)
}

/// A stream the fake back end runs: its file, its mappings, the buffers it holds, and the
/// producer's own counters. The counters are cells, so the threads that look at the configs can
/// share the stream.
struct Stream {
    ring: Mapping,
    configs: Mapping,
    fd: OwnedFd,
    region_fd: OwnedFd,
    pool_fd: OwnedFd,
    config_bytes: usize,
    pool_addr: u64,
    region_addr: u64,
    region: u32,
    jobs_head: Cell<u64>,
    frames_tail: Cell<u64>,
    last_start: Cell<u64>,
    last_end: Cell<u64>,
    /// What the fake hardware says it has done.
    hw_started: Cell<u8>,
    hw_done: Cell<u8>,
}

/// One job: `struct lemnos_pisp_be_job`.
#[derive(Clone, Copy)]
struct Job {
    identity: u64,
    buffer: u32,
    config: u32,
    addr: Addrs,
}

impl Job {
    fn bytes(&self) -> [u8; JOB_BYTES] {
        let mut b = [0u8; JOB_BYTES];
        put64(&mut b, 0, self.identity);
        put32(&mut b, 8, self.buffer);
        put32(&mut b, 12, self.config);
        for (i, a) in self.addr.iter().enumerate() {
            put64(&mut b, 16 + 8 * i, *a);
        }
        b
    }
}

/// `struct lemnos_stream_status`.
struct StreamStatus {
    state: u32,
    fault: i32,
    frames: u64,
    jobs: u64,
}

/// The ring's words, as atomics. Empty only for a mapping that is not whole words, which a stream
/// is never given.
fn words(ring: &Mapping) -> &[AtomicU64] {
    ring.as_atomic_words().unwrap_or(&[])
}

impl Stream {
    /// Makes a stream on the fake back end block, with a pool and a region from the CMA heap.
    fn open(env: &Env) -> io::Result<Self> {
        let pool_fd = dma_heap::allocate(env.heap.as_fd(), u64::from(BUFFERS) * BUFFER_BYTES)?;
        let region_fd = dma_heap::allocate(env.heap.as_fd(), REGION_BYTES)?;
        let mut c = [0u8; 80];
        // `struct lemnos_stream_create`: block, flags, buffers, buffer_bytes, frame_slots,
        // job_slots, job_bytes, pool_fd, doorbell_fd (-1), then fd, ring_bytes and pool_addr out.
        put32(&mut c, 0, env.block);
        put32(&mut c, 8, BUFFERS);
        put32(&mut c, 12, BUFFER_BYTES as u32);
        put32(&mut c, 16, FRAME_SLOTS as u32);
        put32(&mut c, 20, JOB_SLOTS as u32);
        put32(&mut c, 24, JOB_BYTES as u32);
        put_i32(&mut c, 28, pool_fd.as_raw_fd());
        put_i32(&mut c, 32, -1);
        // Out: `config_slot_bytes` (56), `hw_version` (60), `config_mmap_offset` (64), `config_bytes`
        // (72).
        let fd = guest::pisp_be_create(env.ctl(), &mut c)?;
        let ring = Mapping::new(fd.as_fd(), get64(&c, 40) as usize, 0, true)?;
        let config_bytes = get64(&c, 72) as usize;
        let configs = Mapping::new(fd.as_fd(), config_bytes, get64(&c, 64), true)?;
        let mut reg = [0u8; 24];
        put_i32(&mut reg, 0, region_fd.as_raw_fd());
        guest::ioctl_bytes(fd.as_fd(), STREAM_REGISTER, &mut reg)?;
        // The fake block's counters carry on across streams, as a real one's do.
        let mut regs = [0u8; 64];
        read_regs(&mut regs);
        let batch = get32(&regs, BE_BATCH as usize);
        Ok(Self {
            ring,
            configs,
            fd,
            region_fd,
            pool_fd,
            config_bytes,
            pool_addr: get64(&c, 48),
            region_addr: get64(&reg, 8),
            region: get32(&reg, 4),
            jobs_head: Cell::new(0),
            frames_tail: Cell::new(0),
            last_start: Cell::new(0),
            last_end: Cell::new(0),
            hw_done: Cell::new(batch as u8),
            hw_started: Cell::new((batch >> 8) as u8),
        })
    }

    fn words(&self) -> &[AtomicU64] {
        words(&self.ring)
    }

    fn ring_word(&self, index: usize) -> u64 {
        self.words()[index].load(Ordering::Relaxed)
    }

    fn ring_store(&self, index: usize, value: u64) {
        self.words()[index].store(value, Ordering::Relaxed);
    }

    /// Writes `c` to config slot `slot`.
    fn put_config(&self, slot: usize, c: &Config) {
        self.configs.write_bytes(slot * CONFIG_BYTES, &c.b);
    }

    /// Posts `job` as the next slot of the ring.
    fn post(&self, job: &Job) {
        let w = self.words();
        let head = self.jobs_head.get();
        let at = JOBS_AT + (head as usize & (JOB_SLOTS - 1)) * JOB_WORDS;
        let bytes = job.bytes();
        w[at].store(JOB_BYTES as u64, Ordering::Relaxed);
        for (i, chunk) in bytes.as_chunks::<8>().0.iter().enumerate() {
            let word = u64::from_ne_bytes(*chunk);
            w[at + 1 + i].store(word, Ordering::Relaxed);
        }
        self.jobs_head.set(head + 1);
        fence(Ordering::Release);
        w[CONTROL + SHM_JOB_HEAD].store(head + 1, Ordering::Relaxed);
    }

    /// `LEMNOS_STREAM_IOC_KICK`: the errno it failed with, or 0.
    fn kick(&self) -> i32 {
        match guest::ioctl_bytes(self.fd.as_fd(), KICK, &mut [0u8; 0]) {
            Ok(()) => 0,
            Err(e) => errno_of(&e),
        }
    }

    fn status(&self) -> Option<StreamStatus> {
        let mut s = [0u8; 40];
        guest::ioctl_bytes(self.fd.as_fd(), STREAM_STATUS, &mut s).ok()?;
        Some(StreamStatus {
            state: get32(&s, 0),
            fault: get32(&s, 4) as i32,
            frames: get64(&s, 8),
            jobs: get64(&s, 32),
        })
    }

    /// `LEMNOS_PISP_BE_IOC_STATUS`: whether the block is wedged, or the request's error.
    fn pisp_wedged(&self) -> io::Result<u32> {
        let mut b = [0u8; 24];
        guest::ioctl_bytes(self.fd.as_fd(), PISP_STATUS, &mut b)?;
        Ok(get32(&b, 4))
    }

    /// `LEMNOS_STREAM_IOC_UNREGISTER` of `region`.
    fn unregister(&self, region: u32) -> io::Result<()> {
        let mut u = [0u8; 8];
        put32(&mut u, 0, region);
        guest::ioctl_bytes(self.fd.as_fd(), STREAM_UNREGISTER, &mut u)
    }

    /// Counts what the hardware did, as the fake block's counters say it.
    fn advance(&self, started: u8, done: u8) {
        self.hw_started
            .set(self.hw_started.get().wrapping_add(started));
        self.hw_done.set(self.hw_done.get().wrapping_add(done));
    }

    /// Writes the block's batch register from the counters.
    fn post_batch(&self) {
        poke(
            BE_BATCH,
            u32::from(self.hw_done.get()) | (u32::from(self.hw_started.get()) << 8),
        );
    }

    /// Reads the fake block's counters from its batch register: the block finishes jobs by itself
    /// (FAKE_DONE), so the counts this stream kept may be behind it.
    fn sync_counters(&self) {
        let mut regs = [0u8; 64];
        read_regs(&mut regs);
        let batch = get32(&regs, BE_BATCH as usize);
        self.hw_done.set(batch as u8);
        self.hw_started.set((batch >> 8) as u8);
    }

    /// The hardware says it started `started` jobs and finished `done`, and interrupts. Only for a job
    /// the block holds (FAKE_HOLD): otherwise the block has finished it already.
    fn complete_job(&self, started: u8, done: u8) {
        self.sync_counters();
        self.advance(started, done);
        self.post_batch();
        poke(BE_INT_STATUS, 1);
        raise_irq();
    }

    /// The next frame: its buffer (-1 if none arrives in time) and its identity. A buffer needs no
    /// giving back: with no reader marking it, the module takes it back when it needs it.
    fn wait_frame(&self) -> (i64, u64) {
        let got = self.wait_frame_for(None);
        (got.buffer, got.identity)
    }

    /// The next frame whose identity is `want` (any frame when None), on the clock. Frames of other
    /// jobs that come first are passed over and counted; the wait ends after `FRAME_WAIT`, however
    /// many times it woke up in between.
    fn wait_frame_for(&self, want: Option<u64>) -> Got {
        let t0 = Instant::now();
        let mut stale = 0;
        loop {
            match shm_frame_read(self.words(), FRAME_SLOTS, self.frames_tail.get()) {
                FrameRead::Frame(f) => {
                    self.last_start.set(f.timestamp_ns);
                    self.last_end.set(f.end_ns);
                    self.frames_tail.set(self.frames_tail.get() + 1);
                    if want.is_none_or(|w| w == f.identity) {
                        return Got {
                            buffer: i64::from(f.buffer),
                            identity: f.identity,
                            stale,
                            elapsed: t0.elapsed(),
                            head: self.words()[SHM_W_HEAD].load(Ordering::Acquire),
                            tail: self.frames_tail.get(),
                        };
                    }
                    stale += 1;
                }
                FrameRead::NotYet | FrameRead::Reused => {
                    let elapsed = t0.elapsed();
                    if elapsed >= FRAME_WAIT {
                        return Got {
                            buffer: -1,
                            identity: 0,
                            stale,
                            elapsed,
                            head: self.words()[SHM_W_HEAD].load(Ordering::Acquire),
                            tail: self.frames_tail.get(),
                        };
                    }
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }
}

/// Opens a stream, or reports `label` as the failed check and gives nothing.
fn open_stream(env: &Env, label: &str) -> Option<Stream> {
    match Stream::open(env) {
        Ok(st) => Some(st),
        Err(_) => {
            fail(label);
            None
        }
    }
}

/// The good job's config (no tiles but the first, which is one RGB888 image in and out).
type Base = fn(&mut Config, &mut Addrs, &Stream);

/// An RGB888 image in, an RGB888 image out, one tile. Slots: input 0 and output 0.
fn cfg_rgb(c: &mut Config, addr: &mut Addrs, st: &Stream) {
    *c = Config::zeroed();
    *addr = [0; ADDRS];
    c.set32(CFG_NUM_TILES, 1);
    c.set32(CFG_RGB_ENABLES, RGB_INPUT | RGB_OUTPUT0);
    c.set_image(CFG_INPUT, rgb_image());
    c.set_image(out_format(0), rgb_image());
    c.set16(tile(0) + T_IN_W, W);
    c.set16(tile(0) + T_IN_H, H);
    c.set16(tile(0) + T_OUT_W, W);
    c.set16(tile(0) + T_OUT_H, H);
    addr[0] = st.region_addr;
    addr[out_addr(0, 0)] = st.pool_addr;
}

/// Bayer in with TDN and stitch, YUV420 planar on both outputs: every slot but input 1 and 2 used.
fn cfg_bayer_full(c: &mut Config, addr: &mut Addrs, st: &Stream) {
    let bayer = Image {
        w: W,
        h: H,
        fmt: FMT_BPS16,
        stride: 128,
        stride2: 0,
    };
    let yuv = yuv_image();
    *c = Config::zeroed();
    *addr = [0; ADDRS];
    c.set32(CFG_NUM_TILES, 1);
    c.set32(
        CFG_BAYER_ENABLES,
        BAYER_INPUT
            | BAYER_TDN_INPUT
            | BAYER_TDN
            | BAYER_TDN_OUTPUT
            | BAYER_STITCH_INPUT
            | BAYER_STITCH
            | BAYER_STITCH_OUTPUT,
    );
    c.set32(CFG_RGB_ENABLES, RGB_OUTPUT0 | RGB_OUTPUT1);
    c.set_image(CFG_INPUT, bayer);
    c.set_image(CFG_TDN_INPUT, bayer);
    c.set_image(CFG_TDN_OUTPUT, bayer);
    c.set_image(CFG_STITCH_INPUT, bayer);
    c.set_image(CFG_STITCH_OUTPUT, bayer);
    c.set_image(out_format(0), yuv);
    c.set_image(out_format(1), yuv);
    c.set16(tile(0) + T_IN_W, W);
    c.set16(tile(0) + T_IN_H, H);
    c.set16(tile(0) + T_OUT_W, W);
    c.set16(tile(0) + T_OUT_H, H);
    c.set16(tile(0) + T_OUT_W + 2, W);
    c.set16(tile(0) + T_OUT_H + 2, H);

    // Output 0 in the job's pool buffer, output 1 in the region.
    let bayer_bytes = 128 * u64::from(H);
    let y_bytes = 64 * u64::from(H);
    let c_bytes = 32 * (u64::from(H) + 1) / 2;
    let mut r = st.region_addr;
    let mut next = |bytes: u64| {
        let a = r;
        r += (bytes + 15) & !15;
        a
    };
    addr[0] = next(bayer_bytes);
    addr[3] = next(bayer_bytes);
    addr[4] = next(bayer_bytes);
    addr[5] = next(bayer_bytes);
    addr[6] = next(bayer_bytes);
    addr[out_addr(0, 0)] = st.pool_addr;
    addr[out_addr(0, 1)] = st.pool_addr + y_bytes;
    addr[out_addr(0, 2)] = st.pool_addr + y_bytes + ((c_bytes + 15) & !15);
    addr[out_addr(1, 0)] = next(y_bytes);
    addr[out_addr(1, 1)] = next(c_bytes);
    addr[out_addr(1, 2)] = next(c_bytes);
}

/// YUV420 planar in on the RGB side: the only way input planes 1 and 2 are used.
fn cfg_yuv_in(c: &mut Config, addr: &mut Addrs, st: &Stream) {
    cfg_rgb(c, addr, st);
    c.set_image(CFG_INPUT, yuv_image());
    addr[0] = st.region_addr;
    addr[1] = st.region_addr + 64 * u64::from(H);
    addr[2] = st.region_addr + 64 * u64::from(H) + 32 * (u64::from(H) + 1) / 2 + 16;
    addr[2] &= !15;
}

/// Posts a job built from `base` with `mutate` applied, and requires this errno from KICK. `label`
/// is the check's text (the C check prints its mutation's expression).
fn refuse(
    env: &Env,
    label: &str,
    base: Base,
    mutate: impl Fn(&mut Config, &mut Addrs, &Stream),
    expected: i32,
) {
    let st = match Stream::open(env) {
        Ok(st) => st,
        Err(e) => {
            fail(&format!("{label}: stream_open {}", strerror(errno_of(&e))));
            return;
        }
    };
    let mut c = Config::zeroed();
    let mut addr = [0u64; ADDRS];
    base(&mut c, &mut addr, &st);
    mutate(&mut c, &mut addr, &st);
    st.put_config(0, &c);
    let job = Job {
        identity: 1,
        buffer: 0,
        config: 0,
        addr,
    };
    st.post(&job);
    let got = st.kick();
    if got != expected {
        fail(&format!(
            "{label}: errno {got} ({}), wanted {expected} ({})",
            strerror(got),
            strerror(expected)
        ));
    } else {
        say(&format!("ok   refused: {label}"));
    }
}

/// `refuse` for a mutation of the config alone (the C check's `REFUSE` statements).
fn refuse_cfg(env: &Env, label: &str, base: Base, mutate: impl Fn(&mut Config), expected: i32) {
    refuse(env, label, base, |c, _, _| mutate(c), expected);
}

/// The address slots with a region or pool address in the bayer job, tried one by one.
const USED_SLOTS: [usize; 11] = [0, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
/// The tile offset that carries each address slot's offset, as (slot, offset in the tile).
const TILE_SLOT_OFFSETS: [(usize, usize); 10] = [
    (0, T_IN_ADDR),
    (1, T_IN_ADDR2),
    (3, T_TDN_IN_ADDR),
    (4, T_STITCH_IN_ADDR),
    (5, T_TDN_OUT_ADDR),
    (6, T_STITCH_OUT_ADDR),
    (7, T_OUT_ADDR),
    (8, T_OUT_ADDR2),
    (10, T_OUT_ADDR + 4),
    (11, T_OUT_ADDR2 + 4),
];

fn refuse_addresses(env: &Env) {
    for slot in USED_SLOTS {
        refuse(
            env,
            &format!("addr[{slot}] past the region"),
            cfg_bayer_full,
            |_, a, st| a[slot] = st.region_addr + REGION_BYTES + (1 << 33),
            EFAULT,
        );
        refuse(
            env,
            &format!("addr[{slot}] extent overruns the region"),
            cfg_bayer_full,
            |_, a, st| a[slot] = st.region_addr + REGION_BYTES - 16,
            EFAULT,
        );
        refuse(
            env,
            &format!("addr[{slot}] missing"),
            cfg_bayer_full,
            |_, a, _| a[slot] = 0,
            EINVAL,
        );
        refuse(
            env,
            &format!("addr[{slot}] wraps"),
            cfg_bayer_full,
            |_, a, _| a[slot] = u64::MAX - 15,
            EOVERFLOW,
        );
        refuse(
            env,
            &format!("addr[{slot}] misaligned"),
            cfg_bayer_full,
            |_, a, _| a[slot] += 1,
            EINVAL,
        );
    }
    // Input planes 1 and 2, which only a YUV input has.
    for slot in [0, 1, 2] {
        refuse(
            env,
            &format!("yuv input addr[{slot}] past the region"),
            cfg_yuv_in,
            |_, a, st| a[slot] = st.region_addr + REGION_BYTES + (1 << 33),
            EFAULT,
        );
        refuse(
            env,
            &format!("yuv input addr[{slot}] extent overruns"),
            cfg_yuv_in,
            |_, a, st| a[slot] = st.region_addr + REGION_BYTES - 16,
            EFAULT,
        );
    }
    // Disabled stages and planes a format doesn't have: never live.
    for slot in [1, 2, 3, 4, 5, 6, 8, 9, 10, 11, 12] {
        refuse(
            env,
            &format!("addr[{slot}] on a disabled stage"),
            cfg_rgb,
            |_, a, st| a[slot] = st.region_addr,
            EINVAL,
        );
    }
    // A range in another buffer of the pool.
    refuse(
        env,
        "output in another pool buffer",
        cfg_rgb,
        |_, a, st| a[out_addr(0, 0)] = st.pool_addr + BUFFER_BYTES,
        EACCES,
    );
    refuse(
        env,
        "output addr wraps",
        cfg_rgb,
        |_, a, _| a[out_addr(0, 0)] = u64::MAX - 15,
        EOVERFLOW,
    );
}

fn refuse_tiles(env: &Env) {
    for (slot, offset) in TILE_SLOT_OFFSETS {
        let base: Base = if slot == 1 {
            cfg_yuv_in
        } else {
            cfg_bayer_full
        };
        refuse_cfg(
            env,
            &format!("tile offset for addr[{slot}] past its plane"),
            base,
            |c| c.set32(tile(0) + offset, 0xffff_ff00),
            EFAULT,
        );
    }
    // Just past the last byte the tile would touch, at the bottom of the plane.
    refuse_cfg(
        env,
        "input tile hangs off the plane by a line",
        cfg_bayer_full,
        |c| c.set32(tile(0) + T_IN_ADDR, 128),
        EFAULT,
    );
}

fn refuse_formats(env: &Env) {
    let rgb = cfg_rgb as Base;
    let yuv = cfg_yuv_in as Base;
    refuse_cfg(
        env,
        "c->config.input_format.stride = 96",
        rgb,
        |c| c.set_i32(CFG_INPUT + F_STRIDE, 96),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.input_format.stride = 193",
        rgb,
        |c| c.set_i32(CFG_INPUT + F_STRIDE, 193),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.input_format.stride = -192",
        rgb,
        |c| c.set_i32(CFG_INPUT + F_STRIDE, -192),
        EOPNOTSUPP,
    );
    refuse_cfg(
        env,
        "c->config.input_format.stride = 0",
        rgb,
        |c| c.set_i32(CFG_INPUT + F_STRIDE, 0),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.input_format.stride2 = 64",
        rgb,
        |c| c.set_i32(CFG_INPUT + F_STRIDE2, 64),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.input_format.width = 0",
        rgb,
        |c| c.set16(CFG_INPUT + F_W, 0),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.input_format.height = 0",
        rgb,
        |c| c.set16(CFG_INPUT + F_H, 0),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.input_format.format |= 1u << 31",
        rgb,
        |c| c.or32(CFG_INPUT + F_FMT, 1 << 31),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.input_format.format |= PISP_IMAGE_FORMAT_WALLPAPER_ROLL",
        rgb,
        |c| c.or32(CFG_INPUT + F_FMT, FMT_WALLPAPER),
        EOPNOTSUPP,
    );
    refuse_cfg(
        env,
        "c->config.input_format.format |= PISP_IMAGE_FORMAT_HOG_SIGNED",
        rgb,
        |c| c.or32(CFG_INPUT + F_FMT, FMT_HOG_SIGNED),
        EOPNOTSUPP,
    );
    refuse_cfg(
        env,
        "c->config.input_format.format |= PISP_IMAGE_FORMAT_INTEGRAL_IMAGE",
        rgb,
        |c| c.or32(CFG_INPUT + F_FMT, FMT_INTEGRAL),
        EOPNOTSUPP,
    );
    refuse_cfg(
        env,
        "c->config.input_format.format = PISP_IMAGE_FORMAT_THREE_CHANNEL | PISP_IMAGE_FORMAT_SAMPLING_420",
        rgb,
        |c| c.set32(CFG_INPUT + F_FMT, FMT_THREE | FMT_SAMPLING420),
        EOPNOTSUPP,
    );
    refuse_cfg(
        env,
        "c->config.input_format.format = PISP_IMAGE_FORMAT_PLANARITY_PLANAR",
        rgb,
        |c| c.set32(CFG_INPUT + F_FMT, FMT_PLANAR),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.input_format.format = PISP_IMAGE_FORMAT_BPS_16 | PISP_IMAGE_FORMAT_COMPRESSION_MODE_1",
        rgb,
        |c| c.set32(CFG_INPUT + F_FMT, FMT_BPS16 | FMT_COMPRESSION1),
        EOPNOTSUPP,
    );
    refuse_cfg(
        env,
        "c->config.input_format.stride2 = 0",
        yuv,
        |c| c.set_i32(CFG_INPUT + F_STRIDE2, 0),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.input_format.stride2 = -32",
        yuv,
        |c| c.set_i32(CFG_INPUT + F_STRIDE2, -32),
        EOPNOTSUPP,
    );
    refuse_cfg(
        env,
        "c->config.input_format.stride2 = 16",
        yuv,
        |c| c.set_i32(CFG_INPUT + F_STRIDE2, 16),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.output_format[0].image.stride = 193",
        rgb,
        |c| c.set_i32(out_format(0) + F_STRIDE, 193),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.output_format[0].image.stride = 96",
        rgb,
        |c| c.set_i32(out_format(0) + F_STRIDE, 96),
        EINVAL,
    );
    // A taller image is a bigger extent: the image no longer fits its buffer's bytes.
    refuse_cfg(
        env,
        "c->config.input_format.height = 60000",
        rgb,
        |c| c.set16(CFG_INPUT + F_H, 60000),
        EFAULT,
    );
    refuse_cfg(
        env,
        "c->config.output_format[0].image.stride = (1 << 30) - 64 + 64",
        rgb,
        |c| c.set_i32(out_format(0) + F_STRIDE, (1 << 30) - 64 + 64),
        EFAULT,
    );
    refuse_cfg(
        env,
        "c->tiles[0].input_width = 0",
        rgb,
        |c| c.set16(tile(0) + T_IN_W, 0),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->tiles[0].input_height = 0",
        rgb,
        |c| c.set16(tile(0) + T_IN_H, 0),
        EINVAL,
    );
    // Past the image width.
    refuse_cfg(
        env,
        "c->tiles[0].input_offset_x = 1",
        rgb,
        |c| c.set16(tile(0) + T_IN_X, 1),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->tiles[0].input_offset_y = 1",
        rgb,
        |c| c.set16(tile(0) + T_IN_Y, 1),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->tiles[0].output_width[0] = W + 16",
        rgb,
        |c| c.set16(tile(0) + T_OUT_W, W + 16),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->tiles[0].output_offset_y[0] = 8",
        rgb,
        |c| c.set16(tile(0) + T_OUT_Y, 8),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->tiles[0].output_height[0] = 0",
        rgb,
        |c| c.set16(tile(0) + T_OUT_H, 0),
        EINVAL,
    );
}

fn refuse_enables(env: &Env) {
    let rgb = cfg_rgb as Base;
    let bayer = cfg_bayer_full as Base;
    refuse_cfg(
        env,
        "c->num_tiles = 0",
        rgb,
        |c| c.set32(CFG_NUM_TILES, 0),
        EINVAL,
    );
    // Every real tile good, so only the count can refuse it (a walk past the array is KASAN's).
    refuse_cfg(
        env,
        "for (int i = 1; i < PISP_BACK_END_NUM_TILES; i++) c->tiles[i] = c->tiles[0]; c->num_tiles = PISP_BACK_END_NUM_TILES + 1",
        rgb,
        |c| {
            for i in 1..64 {
                c.copy_tile(0, i);
            }
            c.set32(CFG_NUM_TILES, 64 + 1);
        },
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->num_tiles = 0x80000000u",
        rgb,
        |c| c.set32(CFG_NUM_TILES, 0x8000_0000),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.global.rgb_enables &= ~PISP_BE_RGB_ENABLE_INPUT",
        rgb,
        |c| c.and32(CFG_RGB_ENABLES, !RGB_INPUT),
        EINVAL,
    );
    // Both: a bayer input on an RGB job.
    refuse_cfg(
        env,
        "c->config.global.bayer_enables = PISP_BE_BAYER_ENABLE_INPUT",
        rgb,
        |c| c.set32(CFG_BAYER_ENABLES, BAYER_INPUT),
        EINVAL,
    );
    // A Bayer stage, no Bayer input.
    refuse_cfg(
        env,
        "c->config.global.bayer_enables = PISP_BE_BAYER_ENABLE_TDN",
        rgb,
        |c| c.set32(CFG_BAYER_ENABLES, BAYER_TDN),
        EINVAL,
    );
    // A bit that isn't there.
    refuse_cfg(
        env,
        "c->config.global.rgb_enables |= 0x40u",
        rgb,
        |c| c.or32(CFG_RGB_ENABLES, 0x40),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.global.rgb_enables |= PISP_BE_RGB_ENABLE_HOG",
        rgb,
        |c| c.or32(CFG_RGB_ENABLES, RGB_HOG),
        EOPNOTSUPP,
    );
    // TDN input without TDN.
    refuse_cfg(
        env,
        "c->config.global.bayer_enables &= ~PISP_BE_BAYER_ENABLE_TDN",
        bayer,
        |c| c.and32(CFG_BAYER_ENABLES, !BAYER_TDN),
        EINVAL,
    );
    // TDN, no input, not reset.
    refuse_cfg(
        env,
        "c->config.global.bayer_enables &= ~PISP_BE_BAYER_ENABLE_TDN_INPUT",
        bayer,
        |c| c.and32(CFG_BAYER_ENABLES, !BAYER_TDN_INPUT),
        EINVAL,
    );
    // Reset with an input.
    refuse_cfg(
        env,
        "c->config.tdn.reset = 1",
        bayer,
        |c| c.set8(CFG_TDN + 10, 1),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.global.bayer_enables &= ~PISP_BE_BAYER_ENABLE_STITCH",
        bayer,
        |c| c.and32(CFG_BAYER_ENABLES, !BAYER_STITCH),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.global.bayer_enables &= ~PISP_BE_BAYER_ENABLE_STITCH_INPUT",
        bayer,
        |c| c.and32(CFG_BAYER_ENABLES, !BAYER_STITCH_INPUT),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.global.bayer_enables |= PISP_BE_BAYER_ENABLE_STITCH_COMPRESS; c->config.global.bayer_enables &= ~PISP_BE_BAYER_ENABLE_STITCH_OUTPUT",
        bayer,
        |c| {
            c.or32(CFG_BAYER_ENABLES, BAYER_STITCH_COMPRESS);
            c.and32(CFG_BAYER_ENABLES, !BAYER_STITCH_OUTPUT);
        },
        EINVAL,
    );
    // Compressing is in 8-pixel units: the format has to say so or the sizes are wrong.
    refuse_cfg(
        env,
        "c->config.global.bayer_enables |= PISP_BE_BAYER_ENABLE_DECOMPRESS",
        bayer,
        |c| c.or32(CFG_BAYER_ENABLES, BAYER_DECOMPRESS),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.input_format.format = PISP_IMAGE_FORMAT_BPS_8 | PISP_IMAGE_FORMAT_COMPRESSION_MODE_1",
        bayer,
        |c| c.set32(CFG_INPUT + F_FMT, FMT_BPS8 | FMT_COMPRESSION1),
        EINVAL,
    );
    // Stalls the block.
    refuse_cfg(
        env,
        "c->config.input_format.format = PISP_IMAGE_FORMAT_BPS_8",
        bayer,
        |c| c.set32(CFG_INPUT + F_FMT, FMT_BPS8),
        EOPNOTSUPP,
    );
    refuse_cfg(
        env,
        "c->config.global.bayer_enables |= PISP_BE_BAYER_ENABLE_TDN_DECOMPRESS",
        bayer,
        |c| c.or32(CFG_BAYER_ENABLES, BAYER_TDN_DECOMPRESS),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.global.bayer_enables |= PISP_BE_BAYER_ENABLE_TDN_COMPRESS",
        bayer,
        |c| c.or32(CFG_BAYER_ENABLES, BAYER_TDN_COMPRESS),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.global.bayer_enables |= PISP_BE_BAYER_ENABLE_STITCH_DECOMPRESS",
        bayer,
        |c| c.or32(CFG_BAYER_ENABLES, BAYER_STITCH_DECOMPRESS),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.global.bayer_enables |= PISP_BE_BAYER_ENABLE_STITCH_COMPRESS",
        bayer,
        |c| c.or32(CFG_BAYER_ENABLES, BAYER_STITCH_COMPRESS),
        EINVAL,
    );
    // A vertical flip walks up from the offset, which the extent check doesn't cover.
    refuse_cfg(
        env,
        "c->config.output_format[0].transform = PISP_BE_TRANSFORM_VFLIP",
        rgb,
        |c| c.set8(out_format(0) + OUT_TRANSFORM, TRANSFORM_VFLIP),
        EOPNOTSUPP,
    );
    refuse_cfg(
        env,
        "c->config.output_format[0].transform = 0x80",
        rgb,
        |c| c.set8(out_format(0) + OUT_TRANSFORM, 0x80),
        EOPNOTSUPP,
    );
    // Fields the module doesn't drive, even zero-valued fields it won't write.
    refuse_cfg(env, "((uint8_t *)c)[0] = 1", rgb, |c| c.set8(0, 1), EINVAL);
    refuse_cfg(
        env,
        "((uint8_t *)c)[100] = 1",
        rgb,
        |c| c.set8(100, 1),
        EINVAL,
    );
    refuse_cfg(
        env,
        "((uint8_t *)c)[108] = 1",
        rgb,
        |c| c.set8(108, 1),
        EINVAL,
    );
    refuse_cfg(
        env,
        "c->config.axi.r_qos = 1",
        rgb,
        |c| c.set8(CFG_AXI, 1),
        EINVAL,
    );
    refuse_cfg(
        env,
        "((uint8_t *)&c->config)[sizeof(c->config) - 1] = 1",
        rgb,
        |c| c.set8(TILES - 1, 1),
        EINVAL,
    );
}

/// The check's refusals of the stream's own ring and of the mappings the stream gives.
fn refuse_ring(env: &Env) {
    let mut c = Config::zeroed();
    let mut addr = [0u64; ADDRS];
    let mut job = Job {
        identity: 1,
        buffer: 0,
        config: 0,
        addr,
    };

    // A config slot past the last.
    let Some(st) = open_stream(env, "!stream_open(&st)") else {
        return;
    };
    cfg_rgb(&mut c, &mut addr, &st);
    st.put_config(0, &c);
    job.addr = addr;
    job.config = JOB_SLOTS as u32;
    st.post(&job);
    check(st.kick() == EINVAL, "got == EINVAL");
    drop(st);

    // A buffer past the last.
    let Some(st) = open_stream(env, "!stream_open(&st)") else {
        return;
    };
    cfg_rgb(&mut c, &mut addr, &st);
    st.put_config(0, &c);
    job.addr = addr;
    job.config = 0;
    job.buffer = BUFFERS;
    st.post(&job);
    check(st.kick() == EINVAL, "got == EINVAL");
    drop(st);

    // A slot length that isn't the job's.
    let Some(st) = open_stream(env, "!stream_open(&st)") else {
        return;
    };
    cfg_rgb(&mut c, &mut addr, &st);
    st.put_config(0, &c);
    job.buffer = 0;
    st.post(&job);
    st.ring_store(JOBS_AT, (JOB_BYTES - 8) as u64);
    check(st.kick() == EMSGSIZE, "got == EMSGSIZE");
    check(
        st.status()
            .is_some_and(|s| s.state == STATE_FAULT && s.fault == EMSGSIZE),
        "!status_of(&st, &status) && status.state == LEMNOS_RING_STATE_FAULT && status.fault == EMSGSIZE",
    );
    drop(st);

    // A head that can't be.
    let Some(st) = open_stream(env, "!stream_open(&st)") else {
        return;
    };
    st.ring_store(CONTROL + SHM_JOB_HEAD, 1000);
    check(st.kick() == ERANGE, "got == ERANGE");
    drop(st);

    // A job naming a buffer already queued on the block.
    let Some(st) = open_stream(env, "!stream_open(&st)") else {
        return;
    };
    cfg_rgb(&mut c, &mut addr, &st);
    st.put_config(0, &c);
    job.buffer = 0;
    st.post(&job);
    fake_mode(FAKE_HOLD);
    check(st.kick() == 0, "!kick(&st)");
    st.post(&job);
    check(st.kick() == EBUSY, "got == EBUSY");
    st.complete_job(1, 1);
    fake_mode(FAKE_DONE);
    check(st.wait_frame().0 == -1, "wait_frame(&st, NULL) == -1");
    drop(st);

    // The ring and config-slot mappings are shared; a private one is refused.
    let Some(st) = open_stream(env, "!stream_open(&st)") else {
        return;
    };
    check(
        guest::private_mapping(st.fd.as_fd(), st.config_bytes, 0x1000 << 12).is_err(),
        "m == MAP_FAILED",
    );
    check(
        Mapping::new(st.fd.as_fd(), st.config_bytes / 2, 0x1000 << 12, false).is_err(),
        "m == MAP_FAILED",
    );
    check(
        guest::mapping_probe(
            st.fd.as_fd(),
            st.config_bytes,
            0x1000 << 12,
            libc::PROT_READ | libc::PROT_EXEC,
            libc::MAP_SHARED,
        )
        .is_err(),
        "m == MAP_FAILED",
    );
    drop(st);

    // Registers can't be mapped from a block this module drives.
    let mut info = [0u8; 32];
    put32(&mut info, 0, env.block);
    check(
        guest::ioctl_bytes(env.ctl(), IOC_INFO, &mut info).is_ok(),
        "!ioctl(ctl_fd, LEMNOS_IOC_INFO, &info)",
    );
    check(
        get32(&info, 8) & INFO_MMIO_MAPPABLE == 0,
        "!(info.flags & LEMNOS_INFO_MMIO_MAPPABLE)",
    );
    check(get32(&info, 16) == 0, "info.irqs == 0");
}

fn fixture_fill(c: &mut Config, addr: &mut Addrs, region: u64) {
    *c = Config::zeroed();
    *addr = [0; ADDRS];
    for &(at, value) in FIXTURE {
        c.set32(at, value);
    }
    addr[0] = region;
    addr[out_addr(0, 0)] = region + OUTPUT_LUMA;
    addr[out_addr(0, 1)] = region + OUTPUT_CHROMA;
}

/// A job that runs: the registers say what was given, and the frame comes out with the job's
/// identity and buffer.
fn test_runs_and_registers(env: &Env) {
    let mut regs = [0u8; 8192];
    let mut regs2 = [0u8; 8192];
    let Some(st) = open_stream(env, "!stream_open(&st)") else {
        return;
    };
    let mut c = Config::zeroed();
    let mut addr = [0u64; ADDRS];
    cfg_bayer_full(&mut c, &mut addr, &st);
    addr[out_addr(0, 0)] = st.pool_addr + 2 * BUFFER_BYTES;
    addr[out_addr(0, 1)] = st.pool_addr + 2 * BUFFER_BYTES + 64 * u64::from(H);
    addr[out_addr(0, 2)] = st.pool_addr + 2 * BUFFER_BYTES + 64 * u64::from(H) + 512;
    c.set8(CFG_BAYER_ORDER, 0x5a);
    // Wider than the image, fine.
    c.set_i32(CFG_INPUT + F_STRIDE, 256);
    st.put_config(1, &c);
    let job = Job {
        identity: 0xabcd_0001,
        buffer: 2,
        config: 1,
        addr,
    };
    st.post(&job);
    // The job stays on the block while its registers are read (FAKE_HOLD).
    fake_mode(FAKE_HOLD);
    check(st.kick() == 0, "kick(&st) == 0");

    read_regs(&mut regs);
    for (i, want) in addr.iter().enumerate() {
        let got = u64::from(get32(&regs, BE_CONFIG as usize + 8 * i))
            | (u64::from(get32(&regs, BE_CONFIG as usize + 8 * i + 4)) << 32);
        if got != *want {
            fail(&format!("slot {i}: wrote {got:x}, asked {want:x}"));
        }
    }
    check(
        get32(&regs, 0xb0) == c.get32(CFG_BAYER_ENABLES),
        "reg32(regs, 0xb0) == c.config.global.bayer_enables",
    );
    check(
        get32(&regs, 0xb4) == c.get32(CFG_RGB_ENABLES),
        "reg32(regs, 0xb4) == c.config.global.rgb_enables",
    );
    check(
        get32(&regs, BE_CONFIG as usize + CFG_INPUT + F_STRIDE) == 256,
        "reg32(regs, BE_CONFIG + offsetof(struct pisp_be_config, input_format) + 8) == 256",
    );
    check(
        get32(&regs, BE_CONFIG as usize + CFG_BAYER_ORDER) & 0xff == 0x5a,
        "(reg32(regs, BE_CONFIG + offsetof(struct pisp_be_config, global) + 8) & 0xff) == 0x5a",
    );
    // Copy, queue, one tile.
    check(
        get32(&regs, BE_CONTROL as usize) == (3 | (1 << 16)),
        "reg32(regs, BE_CONTROL) == (3u | (1u << 16))",
    );
    check(
        get32(&regs, BE_TILE_LO as usize) != 0,
        "reg32(regs, BE_TILE_LO) != 0",
    );

    // The config changes under the job: nothing the block has changes.
    let all_ff = [0xffu8; CONFIG_BYTES * JOB_SLOTS];
    st.configs.write_bytes(0, &all_ff);
    read_regs(&mut regs2);
    check(regs == regs2, "!memcmp(regs, regs2, sizeof(regs))");
    // Neither do the ring and the addresses the job named.
    for i in 0..JOB_WORDS {
        st.ring_store(JOBS_AT + i, u64::MAX);
    }
    read_regs(&mut regs2);
    check(regs == regs2, "!memcmp(regs, regs2, sizeof(regs))");

    // It finishes: a frame comes out with the job's identity and buffer.
    st.complete_job(1, 1);
    fake_mode(FAKE_DONE);
    let (buffer, id) = st.wait_frame();
    check(buffer == 2, "wait_frame(&st, &id) == 2");
    check(id == 0xabcd_0001, "id == 0xabcd0001");
    check(
        st.status()
            .is_some_and(|s| s.state == STATE_OPEN && s.frames == 1 && s.jobs == 1),
        "!status_of(&st, &status) && status.state == LEMNOS_RING_STATE_OPEN && status.frames == 1 && status.jobs == 1",
    );
}

/// What the checks above must not refuse: compression that says so, a horizontal flip.
fn test_accepted_variants(env: &Env) {
    let Some(st) = open_stream(env, "!stream_open(&st)") else {
        return;
    };
    let mut c = Config::zeroed();
    let mut addr = [0u64; ADDRS];
    cfg_bayer_full(&mut c, &mut addr, &st);
    c.or32(CFG_BAYER_ENABLES, BAYER_DECOMPRESS | BAYER_TDN_COMPRESS);
    c.set32(CFG_INPUT + F_FMT, FMT_BPS8 | FMT_COMPRESSION1);
    c.set32(CFG_TDN_OUTPUT + F_FMT, FMT_BPS8 | FMT_COMPRESSION1);
    c.set8(out_format(0) + OUT_TRANSFORM, TRANSFORM_HFLIP);
    st.put_config(0, &c);
    let job = Job {
        identity: 9,
        buffer: 0,
        config: 0,
        addr,
    };
    st.post(&job);
    let got = st.kick();
    if got != 0 {
        fail(&format!(
            "compressed, mirrored job refused: {}",
            strerror(got)
        ));
    } else {
        say("ok   compressed, mirrored job accepted");
    }
    check(
        got != 0 || st.wait_frame().0 == 0,
        "got || wait_frame(&st, NULL) == 0",
    );
}

/// Two jobs: the second queued while the first runs, done and started moving separately.
fn test_counters_two_jobs(env: &Env) {
    let Some(st) = open_stream(env, "!stream_open(&st)") else {
        return;
    };
    let mut c = Config::zeroed();
    let mut addr = [0u64; ADDRS];
    cfg_rgb(&mut c, &mut addr, &st);
    st.put_config(0, &c);
    let mut job = Job {
        identity: 7,
        buffer: 0,
        config: 0,
        addr,
    };
    // The jobs stay on the block until they are completed by hand (FAKE_HOLD).
    fake_mode(FAKE_HOLD);
    st.post(&job);
    check(st.kick() == 0, "!kick(&st)");
    // First started, second job goes on the block while it runs.
    st.complete_job(1, 0);
    addr[out_addr(0, 0)] = st.pool_addr + BUFFER_BYTES;
    job.addr = addr;
    job.buffer = 1;
    job.identity = 8;
    st.put_config(0, &c);
    st.post(&job);
    check(st.kick() == 0, "!kick(&st)");
    // The first finishes, the second starts and finishes together: done +2, started +1.
    st.complete_job(1, 2);
    fake_mode(FAKE_DONE);
    let (b0, id0) = st.wait_frame();
    check(b0 == 0 && id0 == 7, "wait_frame(&st, &id) == 0 && id == 7");
    let (b1, id1) = st.wait_frame();
    check(b1 == 1 && id1 == 8, "wait_frame(&st, &id) == 1 && id == 8");
    // The block's finish is both the frame's time and its end: frame word 5 (the end time) is that
    // stamp, nonzero, and the same as its start.
    check(
        st.last_end.get() != 0 && st.last_end.get() == st.last_start.get(),
        "st.last_end != 0 && st.last_end == st.last_start",
    );
}

/// A real tile layout built by the config library is accepted, and runs.
fn test_fixture_config(env: &Env) {
    let Some(st) = open_stream(env, "!stream_open(&st)") else {
        return;
    };
    let mut c = Config::zeroed();
    let mut addr = [0u64; ADDRS];
    fixture_fill(&mut c, &mut addr, st.region_addr);
    st.put_config(0, &c);
    let job = Job {
        identity: 5,
        buffer: 0,
        config: 0,
        addr,
    };
    st.post(&job);
    let got = st.kick();
    if got != 0 {
        fail(&format!("fixture config refused: {}", strerror(got)));
    } else {
        say(&format!(
            "ok   the library's config ({} tiles) accepted",
            c.get32(CFG_NUM_TILES)
        ));
    }
    check(
        got != 0 || st.wait_frame().0 == 0,
        "got || wait_frame(&st, NULL) == 0",
    );
}

fn refuse_all(env: &Env) {
    refuse_addresses(env);
    refuse_tiles(env);
    refuse_formats(env);
    refuse_enables(env);
    refuse_ring(env);
    test_runs_and_registers(env);
    test_accepted_variants(env);
    test_counters_two_jobs(env);
    test_config_race(env);
    test_fixture_config(env);
}

/// Writes one 8-byte-aligned word's byte, atomically, in the race's configs: only that byte changes.
fn race_store(words: &[AtomicU64], byte: usize, value: u8) {
    let shift = (byte % 8) * 8;
    let mask = !(0xffu64 << shift);
    let v = u64::from(value) << shift;
    let _ = words[byte / 8].try_update(Ordering::Relaxed, Ordering::Relaxed, |w| {
        Some((w & mask) | v)
    });
}

/// A consumer racing the config while it is being read: never an unchecked value on the block.
fn race_writer(words: &[AtomicU64], stop: &AtomicBool) {
    // One byte, so a copy can't tear it into a third value: 0xc1 is misaligned.
    while !stop.load(Ordering::Relaxed) {
        race_store(words, CFG_INPUT + F_STRIDE, 0xc1);
        race_store(words, CFG_INPUT + F_STRIDE, 0xc0);
    }
}

/// One stream's run of jobs with the writer racing its config. True when the stream refused a job
/// (EINVAL) and the caller must open a new one; false when the checks are done.
fn race_epoch(st: &Stream, addr: &Addrs, next: &mut u64, ran: &mut u32, refused: &mut u32) -> bool {
    let stop = AtomicBool::new(false);
    let words = st.configs.as_atomic_words().unwrap_or(&[]);
    thread::scope(|s| {
        s.spawn(|| race_writer(words, &stop));
        let again = loop {
            if *next >= 200 || failures() >= 5 {
                break false;
            }
            let job = Job {
                identity: *next,
                buffer: 0,
                config: 0,
                addr: *addr,
            };
            st.post(&job);
            let got = st.kick();
            if got != 0 {
                if got == EINVAL {
                    // A refused job ends the stream; the caller starts again, with the writer stopped.
                    *refused += 1;
                    *next += 1;
                    break true;
                }
                fail(&format!("race: errno {got}"));
                break false;
            }
            let mut regs = [0u8; 8192];
            read_regs(&mut regs);
            let stride = get32(&regs, BE_CONFIG as usize + CFG_INPUT + F_STRIDE) as i32;
            if stride != 192 {
                fail(&format!("race: the block was given stride {stride}"));
            }
            *ran += 1;
            let got = st.wait_frame_for(Some(*next));
            if got.buffer != 0 || got.identity != *next {
                fail(&format!(
                    "job {next}: no frame for it (wanted buffer 0, identity {next}): got buffer {} identity {:#x} after {} ms, {} stale frames passed, ring head {} tail {}; module: {}",
                    got.buffer,
                    got.identity,
                    got.elapsed.as_millis(),
                    got.stale,
                    got.head,
                    got.tail,
                    module_view()
                ));
            }
            *next += 1;
        };
        stop.store(true, Ordering::Relaxed);
        again
    })
}

/// The config race: 200 jobs whose config's stride is rewritten while they are read.
fn test_config_race(env: &Env) {
    let mut next = 0u64;
    let mut ran = 0u32;
    let mut refused = 0u32;
    loop {
        let Some(st) = open_stream(env, "!stream_open(&st)") else {
            return;
        };
        let mut c = Config::zeroed();
        let mut addr = [0u64; ADDRS];
        cfg_rgb(&mut c, &mut addr, &st);
        st.put_config(0, &c);
        let again = race_epoch(&st, &addr, &mut next, &mut ran, &mut refused);
        drop(st);
        if !again {
            break;
        }
    }
    say(&format!(
        "ok   config race: {ran} ran with a checked value, {refused} refused"
    ));
    check(ran > 0, "ran > 0");
}

fn test_lost_interrupt(env: &Env) {
    let Some(st) = open_stream(env, "!stream_open(&st)") else {
        return;
    };
    let mut c = Config::zeroed();
    let mut addr = [0u64; ADDRS];
    cfg_rgb(&mut c, &mut addr, &st);
    st.put_config(0, &c);
    let job = Job {
        identity: 9,
        buffer: 0,
        config: 0,
        addr,
    };
    // The block finishes the job and raises no interrupt (FAKE_LOSE). The watchdog (1 s) sees the
    // block idle and finishes it.
    fake_mode(FAKE_LOSE);
    st.post(&job);
    check(st.kick() == 0, "!kick(&st)");
    check(st.wait_frame().0 == 0, "wait_frame(&st, NULL) == 0");
    fake_mode(FAKE_DONE);
    check(
        st.status().is_some_and(|s| s.state == STATE_OPEN),
        "!status_of(&st, &status) && status.state == LEMNOS_RING_STATE_OPEN",
    );
}

/// A status that says a job is still queued keeps the next one off the block.
fn test_queued_bit_blocks_writing(env: &Env) {
    let mut regs = [0u8; 8192];
    let Some(st) = open_stream(env, "!stream_open(&st)") else {
        return;
    };
    let mut c = Config::zeroed();
    let mut addr = [0u64; ADDRS];
    cfg_rgb(&mut c, &mut addr, &st);
    st.put_config(0, &c);
    let job = Job {
        identity: 3,
        buffer: 0,
        config: 0,
        addr,
    };
    // The block says something is queued.
    poke(BE_STATUS, 1);
    poke(BE_CONTROL, 0);
    st.post(&job);
    check(st.kick() == 0, "!kick(&st)");
    read_regs(&mut regs);
    check(
        get32(&regs, BE_CONTROL as usize) == 0,
        "reg32(regs, BE_CONTROL) == 0",
    );
    poke(BE_STATUS, 0);
    // Once the block is idle (watchdog in 1 s) the job goes on it.
    thread::sleep(Duration::from_millis(1300));
    read_regs(&mut regs);
    check(
        get32(&regs, BE_CONTROL as usize) & 1 != 0,
        "reg32(regs, BE_CONTROL) & 1",
    );
    check(st.wait_frame().0 == 0, "wait_frame(&st, NULL) == 0");
}

/// Registered memory can't be released under a job.
fn test_unregister_busy(env: &Env) {
    let Some(st) = open_stream(env, "!stream_open(&st)") else {
        return;
    };
    let mut c = Config::zeroed();
    let mut addr = [0u64; ADDRS];
    cfg_rgb(&mut c, &mut addr, &st);
    st.put_config(0, &c);
    let job = Job {
        identity: 3,
        buffer: 0,
        config: 0,
        addr,
    };
    fake_mode(FAKE_HOLD);
    st.post(&job);
    check(st.kick() == 0, "!kick(&st)");
    let region = st.region;
    check(
        matches!(st.unregister(region), Err(ref e) if errno_of(e) == EBUSY),
        "ioctl(st.fd, LEMNOS_STREAM_IOC_UNREGISTER, &u) == -1 && errno == EBUSY",
    );
    st.complete_job(1, 1);
    fake_mode(FAKE_DONE);
    check(st.wait_frame().0 == 0, "wait_frame(&st, NULL) == 0");
    check(
        st.unregister(region).is_ok(),
        "!ioctl(st.fd, LEMNOS_STREAM_IOC_UNREGISTER, &u)",
    );
    // What the wedged case must not do is what the healthy case does.
    let tiles = last_tiles();
    check(page_refs(tiles) > 0, "page_refs(tiles) > 0");
    drop(st);
    check(page_refs(tiles) == 0, "page_refs(tiles) == 0");
}

/// The thread that completes the job later, as the hardware would: after the caller's close or
/// unbind has started.
fn complete_later(st: &Stream) -> Option<thread::JoinHandle<()>> {
    st.sync_counters();
    let started = st.hw_started.get().wrapping_add(1);
    let done = st.hw_done.get().wrapping_add(1);
    let spawned = thread::Builder::new().spawn(move || {
        thread::sleep(Duration::from_millis(300));
        poke(BE_BATCH, u32::from(done) | (u32::from(started) << 8));
        poke(BE_INT_STATUS, 1);
        raise_irq();
    });
    match spawned {
        Ok(handle) => Some(handle),
        Err(_) => {
            fail("pthread_create");
            None
        }
    }
}

fn join(handle: Option<thread::JoinHandle<()>>) {
    if let Some(handle) = handle {
        let _ = handle.join();
    }
}

/// Closing waits for what's on the block and then lets go of everything.
fn test_close_waits(env: &Env) {
    let Some(st) = open_stream(env, "!stream_open(&st)") else {
        return;
    };
    let mut c = Config::zeroed();
    let mut addr = [0u64; ADDRS];
    cfg_rgb(&mut c, &mut addr, &st);
    st.put_config(0, &c);
    let job = Job {
        identity: 3,
        buffer: 0,
        config: 0,
        addr,
    };
    fake_mode(FAKE_HOLD);
    st.post(&job);
    check(st.kick() == 0, "!kick(&st)");
    let (Ok(pool), Ok(region)) = (st.pool_fd.try_clone(), st.region_fd.try_clone()) else {
        return;
    };
    let later = complete_later(&st);
    let Stream {
        ring,
        configs,
        fd,
        region_fd,
        pool_fd,
        ..
    } = st;
    let t0 = Instant::now();
    drop(ring);
    drop(configs);
    drop(fd);
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    join(later);
    fake_mode(FAKE_DONE);

    say(&format!(
        "ok   close took {ms:.0} ms with a job on the block"
    ));
    // It waited for the job, and not for the timeout.
    check(ms >= 250.0, "ms >= 250");
    check(ms < 1500.0, "ms < 1500");

    // Nothing held any more: those dups are the last references.
    drop(region_fd);
    drop(pool_fd);
    check(
        dmabuf_count(pool.as_raw_fd()) == 1,
        "dmabuf_count(pool) == 1",
    );
    check(
        dmabuf_count(region.as_raw_fd()) == 1,
        "dmabuf_count(region) == 1",
    );
}

/// The stream can't get its fd, with the producer already attached: the descriptor limit is at
/// the next free number, so the block refuses the stream's file with EMFILE.
fn test_create_without_fd(env: &Env) {
    let Ok(pool) = dma_heap::allocate(env.heap.as_fd(), u64::from(BUFFERS) * BUFFER_BYTES) else {
        return;
    };
    let Ok(dup) = pool.try_clone() else {
        return;
    };
    let next = dup.as_raw_fd();
    drop(dup);
    let Ok(old) = guest::nofile_limits() else {
        return;
    };

    let mut c = [0u8; 80];
    put32(&mut c, 0, env.block);
    put32(&mut c, 8, BUFFERS);
    put32(&mut c, 12, BUFFER_BYTES as u32);
    put32(&mut c, 16, FRAME_SLOTS as u32);
    put32(&mut c, 20, JOB_SLOTS as u32);
    put32(&mut c, 24, JOB_BYTES as u32);
    put_i32(&mut c, 28, pool.as_raw_fd());
    put_i32(&mut c, 32, -1);
    let _ = guest::set_nofile_limits(next as u64, old.1);
    let created = guest::pisp_be_create(env.ctl(), &mut c);
    check(
        matches!(created, Err(ref e) if errno_of(e) == EMFILE),
        "no fd for the stream: refused",
    );
    let _ = guest::set_nofile_limits(old.0, old.1);
    drop(pool);
    let reopened = Stream::open(env);
    check(
        reopened.is_ok(),
        "the block is free again after a stream that never existed",
    );
}

fn test_close_idle(env: &Env) {
    let Some(st) = open_stream(env, "!stream_open(&st)") else {
        return;
    };
    let (Ok(pool), Ok(region)) = (st.pool_fd.try_clone(), st.region_fd.try_clone()) else {
        return;
    };
    drop(st);
    check(
        dmabuf_count(pool.as_raw_fd()) == 1,
        "dmabuf_count(pool) == 1",
    );
    check(
        dmabuf_count(region.as_raw_fd()) == 1,
        "dmabuf_count(region) == 1",
    );
}

/// Unbinding with a job on the block waits for it and ends the stream.
fn test_unbind_waits(env: &Env, bind: &str, unbind: &str, device: &str) {
    let Some(st) = open_stream(env, "!stream_open(&st)") else {
        return;
    };
    let mut c = Config::zeroed();
    let mut addr = [0u64; ADDRS];
    cfg_rgb(&mut c, &mut addr, &st);
    st.put_config(0, &c);
    let job = Job {
        identity: 3,
        buffer: 0,
        config: 0,
        addr,
    };
    fake_mode(FAKE_HOLD);
    st.post(&job);
    check(st.kick() == 0, "!kick(&st)");
    let later = complete_later(&st);
    let unbound = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(unbind)
        .is_ok_and(|mut f| write_len(&mut f, device.as_bytes()));
    check(
        unbound,
        "fd >= 0 && write(fd, device, strlen(device)) == (ssize_t)strlen(device)",
    );
    join(later);
    fake_mode(FAKE_DONE);
    // The stream's over: no use-after-free in the module's status path.
    check(
        st.status().is_some_and(|s| s.state == STATE_CLOSED),
        "!status_of(&st, &status) && status.state == LEMNOS_RING_STATE_CLOSED",
    );
    check(st.kick() == ENODEV, "kick(&st) == ENODEV");
    check(
        matches!(st.pisp_wedged(), Err(ref e) if errno_of(e) == ENODEV),
        "ioctl(st.fd, LEMNOS_PISP_BE_IOC_STATUS, &pb) == -1 && errno == ENODEV",
    );
    drop(st);
    // Re-bind.
    check(
        write_node(bind, device.as_bytes()),
        "fd >= 0 && write(fd, device, strlen(device)) == (ssize_t)strlen(device)",
    );
}

/// Closes a stream and requires its buffers to stay held: the wedged block's buffers are pinned
/// after the stream is gone, and the labelled check says so.
fn close_and_expect_pinned(st: Stream, what: &str) {
    let (Ok(pool), Ok(region)) = (st.pool_fd.try_clone(), st.region_fd.try_clone()) else {
        return;
    };
    let Stream {
        ring,
        configs,
        fd,
        region_fd,
        pool_fd,
        ..
    } = st;
    drop(ring);
    drop(configs);
    drop(fd);
    drop(region_fd);
    drop(pool_fd);
    // Our dups plus the kernel's references: the stream is gone but what it held is not. The
    // pool's reference stays too: the job might be writing it.
    let (pool_count, region_count) = (
        dmabuf_count(pool.as_raw_fd()),
        dmabuf_count(region.as_raw_fd()),
    );
    if pool_count < 2 || region_count < 2 {
        fail(&format!(
            "{what}: buffers released (count {pool_count}, {region_count})"
        ));
    }
}

fn test_wedge_timeout(env: &Env) {
    let Some(st) = open_stream(env, "!stream_open(&st)") else {
        return;
    };
    let mut c = Config::zeroed();
    let mut addr = [0u64; ADDRS];
    cfg_rgb(&mut c, &mut addr, &st);
    st.put_config(0, &c);
    let job = Job {
        identity: 3,
        buffer: 0,
        config: 0,
        addr,
    };
    fake_mode(FAKE_HOLD);
    st.post(&job);
    check(st.kick() == 0, "!kick(&st)");
    // Busy, never finishing.
    poke(BE_STATUS, 2);
    thread::sleep(Duration::from_millis(1400));
    check(
        st.status()
            .is_some_and(|s| s.state == STATE_FAULT && s.fault == ETIMEDOUT),
        "!status_of(&st, &status) && status.state == LEMNOS_RING_STATE_FAULT && status.fault == ETIMEDOUT",
    );
    check(
        st.pisp_wedged().is_ok_and(|w| w == 1),
        "!ioctl(st.fd, LEMNOS_PISP_BE_IOC_STATUS, &pb) && pb.wedged == 1",
    );
    // The block says it finished, too late to be believed: nothing is let go.
    let region = st.region;
    let tiles = last_tiles();
    st.complete_job(1, 1);
    fake_mode(FAKE_DONE);
    thread::sleep(Duration::from_millis(50));
    check(
        matches!(st.unregister(region), Err(ref e) if errno_of(e) == EBUSY),
        "ioctl(st.fd, LEMNOS_STREAM_IOC_UNREGISTER, &u) == -1 && errno == EBUSY",
    );
    close_and_expect_pinned(st, "wedged by timeout");
    // The block may still read its tiles.
    check(page_refs(tiles) > 0, "page_refs(tiles) > 0");

    // Nothing new on a wedged block.
    let reopened = Stream::open(env);
    check(
        matches!(&reopened, Err(e) if errno_of(e) == EIO),
        "stream_open(&st) == -1 && errno == EIO",
    );
}

/// Hardware counters that say more happened than we gave it.
fn test_wedge_counters(env: &Env) {
    let Some(st) = open_stream(env, "!stream_open(&st)") else {
        return;
    };
    let mut c = Config::zeroed();
    let mut addr = [0u64; ADDRS];
    cfg_rgb(&mut c, &mut addr, &st);
    st.put_config(0, &c);
    let job = Job {
        identity: 3,
        buffer: 0,
        config: 0,
        addr,
    };
    fake_mode(FAKE_HOLD);
    st.post(&job);
    check(st.kick() == 0, "!kick(&st)");
    // Claims two done with one job, and three started. Nothing is finished.
    poke(BE_BATCH, 2 | (3 << 8));
    poke(BE_INT_STATUS, 1);
    raise_irq();
    thread::sleep(Duration::from_millis(50));
    check(
        st.status()
            .is_some_and(|s| s.state == STATE_FAULT && s.frames == 0),
        "!status_of(&st, &status) && status.state == LEMNOS_RING_STATE_FAULT && status.frames == 0",
    );
    check(
        st.pisp_wedged().is_ok_and(|w| w == 1),
        "!ioctl(st.fd, LEMNOS_PISP_BE_IOC_STATUS, &pb) && pb.wedged == 1",
    );
    check(st.ring_word(SHM_W_HEAD) == 0, "st.ring[SHM_W_HEAD] == 0");
    fake_mode(FAKE_DONE);
    close_and_expect_pinned(st, "wedged by counters");
}

/// Unbinds and rebinds the device, with the fake block's counters cleared.
fn rebind(bind: &str, unbind: &str, device: &str) {
    if let Ok(mut f) = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(unbind)
    {
        check(
            write_len(&mut f, device.as_bytes()),
            "write(fd, device, strlen(device)) == (ssize_t)strlen(device)",
        );
    }
    fake_mode(FAKE_DONE);
    poke(BE_BATCH, 0);
    poke(BE_STATUS, 0);
    poke(BE_INT_STATUS, 0);
    check(
        write_node(bind, device.as_bytes()),
        "fd >= 0 && write(fd, device, strlen(device)) == (ssize_t)strlen(device)",
    );
}

fn open_rw(path: &str) -> io::Result<OwnedFd> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(path)
        .map(OwnedFd::from)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 7 {
        say("usage");
        return ExitCode::from(2);
    }
    let (node, device, bind, unbind, heap_path, mode) =
        (&args[1], &args[2], &args[3], &args[4], &args[5], &args[6]);

    let ctl = open_rw(node);
    let heap = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(heap_path)
        .map(OwnedFd::from);
    let (ctl, heap) = match (ctl, heap) {
        (Ok(ctl), Ok(heap)) => (ctl, heap),
        (ctl, heap) => {
            // The error is the last open's: the heap's, when it failed.
            let err = match (&ctl, &heap) {
                (_, Err(e)) => errno_of(e),
                (Err(e), Ok(_)) => errno_of(e),
                (Ok(_), Ok(_)) => 0,
            };
            fail(&format!("open {node}/{heap_path}: {}", strerror(err)));
            return ExitCode::from(1);
        }
    };
    let mut env = Env {
        ctl: Some(ctl),
        heap,
        block: 0,
    };
    env.block = find_block(env.ctl(), device);
    if env.block == 0 {
        fail(&format!("no block {device}"));
        return ExitCode::from(1);
    }

    match mode.as_str() {
        "refuse" => refuse_all(&env),
        "lifetime" => {
            test_lost_interrupt(&env);
            test_queued_bit_blocks_writing(&env);
            test_unregister_busy(&env);
            test_create_without_fd(&env);
            test_close_idle(&env);
            test_close_waits(&env);
            test_unbind_waits(&env, bind, unbind, device);
            env.block = find_block(env.ctl(), device);
        }
        "wedge" => {
            test_wedge_counters(&env);
            // The control node is closed for the rebind, as the C check closes it.
            env.ctl = None;
            rebind(bind, unbind, device);
            match open_rw(node) {
                Ok(ctl) => env.ctl = Some(ctl),
                Err(e) => {
                    fail(&format!("open {node}: {}", strerror(errno_of(&e))));
                    return ExitCode::from(1);
                }
            }
            env.block = find_block(env.ctl(), device);
            test_wedge_timeout(&env);
        }
        _ => {}
    }

    let failed = failures();
    drop(env);
    if failed > 0 {
        say(&format!("FAILED: {failed} checks"));
        ExitCode::from(1)
    } else {
        say(&format!("ok   {mode}: all checks passed"));
        ExitCode::SUCCESS
    }
}
