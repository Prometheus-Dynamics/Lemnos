//! The receiver's frames through the host's own frame stream, end to end: a consumer attaches to
//! the `frames` stream as a client of the daemon does and reads each frame through the descriptor
//! it was handed for the buffer. A stand-in for the block and the module writes a known pattern
//! by device address into the memory it was given for a buffer, as the block's DMA does, and
//! keeps the module's rules: a buffer with no memory is never filled, and a buffer's memory
//! can't change while the block is writing it. The consumer must read that pattern: a frame
//! number on the ring over a buffer the block never wrote is the failure this test exists for.
//!
//! Sealed memfds stand in for the stream's dma-bufs.

#![cfg(target_os = "linux")]

use std::collections::VecDeque;
use std::fs::File;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::{Arc, Mutex};

use lemnos_contract_camera_receiver::{CameraReceiver, Counters, FrameStarts, LinkConfig, Packing};
use lemnos_core::{CallerId, Health, Lifecycle, Result, Status, StreamHandle};
use lemnos_driver_rp1_csi2::relay::Set;
use lemnos_driver_rp1_csi2::{Channel, Finished, Hardware, Rp1Csi2, StartParams};
use lemnos_fake_devices::FakeCounter;
use lemnos_host::board::parse;
use lemnos_host::stream::{Buffers, MemfdAllocator, StreamAllocator};
use lemnos_host::{DeviceState, Host, Options as HostOptions, entry};
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_sim::SimBoard;
use lemnos_wire::Planes;
use lemnos_wire::ring::{Consumer, Layout};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 32;
/// RAW10 packed at 64 by 32: 80 bytes a line.
const FRAME_BYTES: u32 = 80 * HEIGHT;
const BUFFERS: u32 = 4;
/// Where the stand-in puts the first memory it is given, and how far apart the rest.
const MEMORY: u64 = 0x4000_0000;
const MEMORY_SPAN: u64 = 0x0010_0000;
const CALLER: CallerId = CallerId(5);

/// What the block writes for the frame numbered `sequence`.
fn pattern(sequence: u64, at: usize) -> u8 {
    (sequence as usize).wrapping_mul(31).wrapping_add(at * 7) as u8 | 1
}

fn image(sequence: u64) -> Vec<u8> {
    (0..FRAME_BYTES as usize)
        .map(|at| pattern(sequence, at))
        .collect()
}

// ---- The streams' memory, as the daemon makes it: one descriptor per buffer.

/// Each buffer of a stream its own sealed memfd, lent as its handle. The plain allocator makes
/// shared memory, as a stream without dma-bufs gets.
struct Handles;

impl StreamAllocator for Handles {
    fn allocate(&self, name: &str, layout: &Layout) -> core::result::Result<Buffers, Status> {
        MemfdAllocator.allocate(name, layout)
    }

    fn allocate_planes(
        &self,
        _name: &str,
        layout: &Layout,
        _heap: &str,
        _planes: &Planes,
    ) -> core::result::Result<Buffers, Status> {
        let mut fds = Vec::new();
        for _ in 0..layout.buffers {
            fds.push(sealed_memfd(c"csi2-frames", layout.stride()).map_err(|_| Status::IO)?);
        }
        Ok(Buffers {
            fds,
            dma: true,
            sync: None,
        })
    }
}

/// A host whose device 0 has a `frames` stream of `buffer_bytes` buffers, `memory` as given.
fn host_with(buffer_bytes: u32, memory: &str) -> Host {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"csi0\"\ndriver = \"fake-counter\"\n\
         [devices.streams.frames]\nbuffers = {BUFFERS}\nbuffer_bytes = {buffer_bytes}\n{memory}"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start_with_streams(
        &parse(&text).unwrap(),
        &[entry::<FakeCounter>()],
        Box::new(sim),
        Arc::new(clock),
        HostOptions::default(),
        Arc::new(Handles),
    )
    .unwrap()
}

fn host() -> Host {
    host_with(FRAME_BYTES, "memory = \"dma-buf\"\n")
}

// ---- The block and the module: fills buffers by device address, keeping the module's rules.

/// Memory the module pinned for a buffer.
struct Pinned {
    addr: u64,
    file: File,
    size: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Where {
    /// The module's, free to be filled.
    Free,
    /// The block is writing it.
    Block,
    /// Published, due back with this fence.
    Out(u32),
}

struct Buffer {
    memory: Option<Pinned>,
    at: Where,
    fence: u32,
}

#[derive(Default)]
struct Block {
    buffers: Vec<Buffer>,
    /// Streams made and started.
    made: u32,
    going: bool,
    frame_bytes: u32,
    cursor: usize,
    /// Frame starts counted since the block was started, as the kernel counts them: a start
    /// is counted when the block has an address to show it, armed or not. The latest one's time
    /// is the count times 1000 ns (the stand-in's clock).
    starts: u64,
    /// Whether the block has had a buffer's address since it was started: until then it raises
    /// no frame start the kernel can count (measured on the board, `testing/board-runs/D5.md`).
    addressed: bool,
    /// The frame being written: its buffer, the address the block was armed with, and its
    /// sequence, which is its own start's count less one.
    writing: Option<(u32, u64, u64)>,
    finished: VecDeque<Finished>,
    /// Memory given, counting swaps.
    given: u32,
    /// Frames whose address was in no memory the module holds.
    stray: u32,
    /// Returns of a buffer that wasn't out, or with the wrong fence.
    bad_returns: u32,
    /// Every dma-buf ever unpinned, kept so a test can see nothing was written to it after.
    unpinned: Vec<File>,
}

impl Block {
    /// A frame starts: the block is armed with the next free buffer that has memory.
    fn begin_frame(&mut self) -> bool {
        if !self.going {
            return false;
        }
        let count = self.buffers.len();
        let free = (0..count)
            .map(|step| (self.cursor + step) % count)
            .find(|&index| {
                let buffer = &self.buffers[index];
                matches!((buffer.at, &buffer.memory), (Where::Free, Some(_)))
            });
        // With no address yet, the block raises no frame start: nothing is counted.
        if free.is_none() && !self.addressed {
            return false;
        }
        // Otherwise every start is counted, a dropped one and one during a frame included.
        let sequence = self.starts;
        self.starts += 1;
        if self.writing.is_some() {
            return false;
        }
        let Some(index) = free else {
            return false;
        };
        let buffer = &mut self.buffers[index];
        buffer.at = Where::Block;
        let addr = buffer.memory.as_ref().map_or(0, |m| m.addr);
        self.writing = Some((index as u32, addr, sequence));
        self.cursor = (index + 1) % count;
        self.addressed = true;
        true
    }

    /// The frame ends: the block has written it at the address it was armed with.
    fn end_frame(&mut self) -> Option<u32> {
        let (index, addr, sequence) = self.writing.take()?;
        let data = image(sequence);
        let wrote = self
            .buffers
            .iter()
            .filter_map(|b| b.memory.as_ref())
            .any(|m| {
                addr == m.addr
                    && data.len() as u64 <= m.size
                    && m.file.write_all_at(&data, 0).is_ok()
            });
        if !wrote {
            self.stray += 1;
        }
        let buffer = &mut self.buffers[index as usize];
        buffer.fence += 1;
        buffer.at = Where::Out(buffer.fence);
        self.finished.push_back(Finished {
            buffer: index,
            bytes: self.frame_bytes,
            sequence,
            timestamp_ns: 1000 + sequence,
            fence: buffer.fence,
        });
        Some(index)
    }

    fn frame(&mut self) -> Option<u32> {
        if self.begin_frame() {
            self.end_frame()
        } else {
            None
        }
    }

    fn with_memory(&self) -> usize {
        self.buffers.iter().filter(|b| b.memory.is_some()).count()
    }
}

#[derive(Clone, Default)]
struct Hw(Arc<Mutex<Block>>);

impl Hw {
    fn with<R>(&self, f: impl FnOnce(&mut Block) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }
}

fn same_file(a: &File, b: &File) -> bool {
    match (a.metadata(), b.metadata()) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

impl Hardware for Hw {
    fn max_lanes(&mut self) -> Result<u8> {
        Ok(2)
    }

    fn make(&mut self, params: &StartParams) -> Result<()> {
        self.with(|b| {
            b.made += 1;
            b.going = false;
            b.frame_bytes = params.size;
            b.writing = None;
            b.finished.clear();
            b.buffers = (0..params.buffers)
                .map(|_| Buffer {
                    memory: None,
                    at: Where::Free,
                    fence: 0,
                })
                .collect();
        });
        Ok(())
    }

    fn go(&mut self) -> Result<()> {
        // START: the count begins again.
        self.with(|b| {
            b.going = true;
            b.starts = 0;
            b.addressed = b.buffers.iter().any(|buffer| buffer.memory.is_some());
        });
        Ok(())
    }

    fn frame_starts(&mut self) -> Result<Option<FrameStarts>> {
        Ok(self.with(|b| {
            b.going.then_some(FrameStarts {
                count: b.starts,
                latest_ns: if b.starts == 0 { 0 } else { b.starts * 1000 },
            })
        }))
    }

    fn stop(&mut self) -> Result<()> {
        self.with(|b| {
            b.going = false;
            b.writing = None;
            for buffer in &mut b.buffers {
                b.unpinned.extend(buffer.memory.take().map(|m| m.file));
            }
            b.buffers.clear();
        });
        Ok(())
    }

    fn check(&mut self) -> Result<()> {
        Ok(())
    }

    fn counters(&mut self) -> Result<Counters> {
        Ok(Counters::default())
    }

    fn set_buffer(&mut self, channel: Channel, buffer: u32, memory: BorrowedFd<'_>) -> Result<Set> {
        // These tests run no embedded stream: the block has none to give memory to.
        if channel == Channel::Embedded {
            return Ok(Set::Refused);
        }
        let file = File::from(memory.try_clone_to_owned().map_err(|_| Status::IO)?);
        let size = file.metadata().map_err(|_| Status::IO)?.len();
        self.with(|b| {
            let frame_bytes = u64::from(b.frame_bytes);
            let addr = MEMORY + u64::from(b.given) * MEMORY_SPAN;
            let Some(slot) = b.buffers.get_mut(buffer as usize) else {
                return Ok(Set::Refused);
            };
            if size < frame_bytes {
                return Ok(Set::Refused);
            }
            if slot
                .memory
                .as_ref()
                .is_some_and(|m| same_file(&m.file, &file))
            {
                return Ok(Set::Done);
            }
            if slot.at == Where::Block {
                return Ok(Set::Busy);
            }
            let old = slot.memory.replace(Pinned { addr, file, size });
            b.unpinned.extend(old.map(|m| m.file));
            b.given += 1;
            Ok(Set::Done)
        })
    }

    fn next_frame(&mut self, channel: Channel) -> Result<Option<Finished>> {
        if channel == Channel::Embedded {
            return Ok(None);
        }
        Ok(self.with(|b| b.finished.pop_front()))
    }

    fn give_back(&mut self, channel: Channel, buffer: u32, fence: u32) -> Result<()> {
        if channel == Channel::Embedded {
            return Ok(());
        }
        self.with(|b| match b.buffers.get_mut(buffer as usize) {
            Some(slot) if slot.at == Where::Out(fence) => slot.at = Where::Free,
            _ => b.bad_returns += 1,
        });
        Ok(())
    }
}

fn link() -> LinkConfig {
    LinkConfig {
        lanes: 2,
        virtual_channel: 0,
        data_type: 0x2b,
        bit_depth: 10,
        continuous_clock: false,
        width: WIDTH,
        height: HEIGHT,
        pixel_rate_hz: 80_000_000,
        link_rate_mbps: 800,
    }
}

fn driver(host: &Host, hw: &Hw) -> Rp1Csi2 {
    let frames: StreamHandle =
        lemnos_core::Held::boxed(Box::new(host.stream_producer(0, "frames").unwrap()));
    let mut receiver = Rp1Csi2::with_hardware(Box::new(hw.clone()), Some(frames));
    let layout = receiver.configure(link(), Packing::Packed).unwrap();
    assert_eq!(layout.size, FRAME_BYTES);
    receiver
}

/// The consumer's end of the stream, as a client holds it.
struct Client {
    handles: Option<Arc<[OwnedFd]>>,
    ring: Consumer,
}

impl Client {
    fn attach(host: &Host) -> Self {
        let lease = host.stream_attach(0, "frames", CALLER).unwrap();
        let handles = lease.handles.clone();
        let ring = lease.consumer().unwrap();
        Self { handles, ring }
    }

    /// The descriptor the consumer was handed for `buffer`, as a file of its own.
    fn file(&self, buffer: u32) -> File {
        let handles = self.handles.as_ref().expect("a stream of dma-bufs");
        File::from(
            handles[buffer as usize]
                .as_fd()
                .try_clone_to_owned()
                .unwrap(),
        )
    }

    /// What the descriptor the consumer was handed for `buffer` holds.
    fn read(&self, buffer: u32) -> Vec<u8> {
        let mut out = vec![0; FRAME_BYTES as usize];
        self.file(buffer).read_exact_at(&mut out, 0).unwrap();
        out
    }
}

fn read(file: &File) -> Vec<u8> {
    let mut out = vec![0; FRAME_BYTES as usize];
    file.read_exact_at(&mut out, 0).unwrap();
    out
}

#[test]
fn the_consumer_reads_what_the_block_wrote() {
    let host = host();
    let hw = Hw::default();
    let mut frames = Client::attach(&host);
    let mut receiver = driver(&host, &hw);
    receiver.start().unwrap();
    assert_eq!(
        hw.with(|b| b.with_memory()),
        BUFFERS as usize,
        "every buffer of the stream was given to the block before it started"
    );

    let mut now = 1;
    for sequence in 0..7u64 {
        let filled = hw.with(Block::frame).expect("a buffer for the frame");
        now = receiver.tick(now).unwrap();
        let frame = frames.ring.pop_frame().unwrap().expect("a frame");
        assert_eq!(
            (frame.buffer, frame.bytes, frame.sequence),
            (filled, FRAME_BYTES, sequence)
        );
        let seen = frames.read(frame.buffer);
        let expected = image(sequence);
        assert!(
            seen == expected,
            "frame {sequence} in buffer {}: the consumer's buffer doesn't hold what the block \
             wrote (first bytes {:?}, expected {:?})",
            frame.buffer,
            &seen[..8],
            &expected[..8]
        );
        assert!(frames.ring.release(frame.buffer));
        now = receiver.tick(now).unwrap();
    }
    assert_eq!(hw.with(|b| (b.stray, b.bad_returns)), (0, 0));
    assert_eq!(receiver.counters().unwrap().dropped, 0);
    assert_eq!(receiver.health(), Health::Ok);
}

#[test]
fn a_buffer_the_consumer_holds_is_not_filled_again() {
    let host = host();
    let hw = Hw::default();
    let mut frames = Client::attach(&host);
    let mut receiver = driver(&host, &hw);
    receiver.start().unwrap();
    let mut now = 1;
    let mut held = Vec::new();
    for sequence in 0..u64::from(BUFFERS) {
        hw.with(Block::frame).expect("a buffer");
        now = receiver.tick(now).unwrap();
        let frame = frames.ring.pop_frame().unwrap().expect("a frame");
        assert_eq!(frame.sequence, sequence);
        held.push(frame);
    }
    // The consumer has all four: the block has nowhere to write, and that start is dropped.
    assert_eq!(hw.with(Block::frame), None);
    now = receiver.tick(now).unwrap();
    assert!(frames.ring.pop_frame().unwrap().is_none());
    assert_eq!(
        receiver.frame_starts().unwrap().map(|fs| fs.count),
        Some(5),
        "the dropped start is counted"
    );
    for frame in &held {
        assert!(
            frames.read(frame.buffer) == image(frame.sequence),
            "untouched"
        );
    }
    // One goes back, and that one is filled next.
    let back = held.remove(2);
    assert!(frames.ring.release(back.buffer));
    now = receiver.tick(now).unwrap();
    assert_eq!(hw.with(Block::frame), Some(back.buffer));
    receiver.tick(now).unwrap();
    let frame = frames.ring.pop_frame().unwrap().expect("a frame");
    // Five starts so far, the dropped one among them: this frame's start is the sixth.
    assert_eq!((frame.buffer, frame.sequence), (back.buffer, 5));
    assert!(frames.read(frame.buffer) == image(5));
    assert_eq!(hw.with(|b| (b.stray, b.bad_returns)), (0, 0));
}

#[test]
fn a_new_consumer_gets_frames_in_its_own_memory() {
    let host = host();
    let hw = Hw::default();
    let mut first = Client::attach(&host);
    let mut receiver = driver(&host, &hw);
    receiver.start().unwrap();
    let mut now = 1;
    hw.with(Block::frame).expect("a buffer");
    now = receiver.tick(now).unwrap();
    let frame = first.ring.pop_frame().unwrap().expect("a frame");
    assert!(first.read(frame.buffer) == image(0));

    // The first consumer leaves holding that frame, with the block in the middle of the next.
    assert!(hw.with(Block::begin_frame));
    let in_flight = hw.with(|b| b.writing.unwrap().0);
    let old: Vec<File> = (0..BUFFERS).map(|buffer| first.file(buffer)).collect();
    drop(first);
    host.stream_detach(0, "frames", CALLER).unwrap();
    let mut second = Client::attach(&host);
    now = receiver.tick(now).unwrap();
    assert_eq!(
        hw.with(|b| b.given),
        2 * BUFFERS - 1,
        "every buffer but the one the block is writing has the second consumer's memory"
    );

    // The frame in flight ends where it started: in the first consumer's memory, which the
    // module still holds. The second consumer is told nothing and its memory is untouched.
    assert_eq!(hw.with(Block::end_frame), Some(in_flight));
    now = receiver.tick(now).unwrap();
    assert!(second.ring.pop_frame().unwrap().is_none());
    assert!(read(&old[in_flight as usize]) == image(1));
    assert!(second.read(in_flight).iter().all(|&byte| byte == 0));
    assert_eq!(receiver.counters().unwrap().dropped, 1);
    assert_eq!(hw.with(|b| b.given), 2 * BUFFERS, "and now that buffer too");

    // From here every frame is the second consumer's, in its own memory, and nothing more is
    // written to the first's.
    let before: Vec<Vec<u8>> = old.iter().map(read).collect();
    for sequence in 2..10u64 {
        let filled = hw.with(Block::frame).expect("a buffer");
        now = receiver.tick(now).unwrap();
        let frame = second.ring.pop_frame().unwrap().expect("a frame");
        assert_eq!((frame.buffer, frame.sequence), (filled, sequence));
        assert!(
            second.read(frame.buffer) == image(sequence),
            "frame {sequence}: the second consumer's buffer {} doesn't hold what the block wrote",
            frame.buffer
        );
        assert!(second.ring.release(frame.buffer));
        now = receiver.tick(now).unwrap();
    }
    let after: Vec<Vec<u8>> = old.iter().map(read).collect();
    assert!(
        before == after,
        "the first consumer's memory was left alone"
    );
    assert_eq!(hw.with(|b| (b.stray, b.bad_returns)), (0, 0));
    assert_eq!(hw.with(|b| b.made), 1, "the block was never stopped for it");
}

#[test]
fn with_no_consumer_the_block_has_no_memory_and_gets_it_when_one_comes() {
    let host = host();
    let hw = Hw::default();
    let mut receiver = driver(&host, &hw);
    receiver.start().unwrap();
    let mut now = 1;
    for _ in 0..3 {
        now = receiver.tick(now).unwrap();
        assert_eq!(hw.with(|b| b.with_memory()), 0);
        assert_eq!(hw.with(Block::frame), None, "nowhere to write");
    }
    let mut frames = Client::attach(&host);
    now = receiver.tick(now).unwrap();
    assert_eq!(hw.with(|b| b.with_memory()), BUFFERS as usize);
    hw.with(Block::frame).expect("a buffer");
    receiver.tick(now).unwrap();
    let frame = frames.ring.pop_frame().unwrap().expect("a frame");
    assert!(frames.read(frame.buffer) == image(0));
}

#[test]
fn a_stream_without_handles_is_refused_with_or_without_a_consumer() {
    // Plain shared memory: nothing to give the block. The stream says so before anyone attaches.
    let host = host_with(FRAME_BYTES, "");
    let hw = Hw::default();
    let mut receiver = driver(&host, &hw);
    assert_eq!(receiver.start().unwrap_err(), Status::NOT_SUPPORTED);
    let mut frames = Client::attach(&host);
    assert_eq!(receiver.start().unwrap_err(), Status::NOT_SUPPORTED);
    assert_eq!(hw.with(|b| (b.going, b.given)), (false, 0));
    assert_eq!(receiver.tick(1), None, "not streaming");
    assert!(frames.ring.pop_frame().unwrap().is_none());
}

/// A board whose device `csi0` is this driver, with `frames` as `memory` says.
fn bound(memory: &str) -> Host {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"csi0\"\ndriver = \"rp1-csi2\"\nconfig = {{ device = \"none\" }}\n\
         [devices.streams.frames]\nbuffers = {BUFFERS}\nbuffer_bytes = {FRAME_BYTES}\n{memory}"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    Host::start_with_streams(
        &parse(&text).unwrap(),
        &[entry::<Rp1Csi2>()],
        Box::new(sim),
        Arc::new(clock),
        HostOptions::default(),
        Arc::new(Handles),
    )
    .unwrap()
}

#[test]
fn a_board_whose_frames_stream_is_plain_memory_is_refused_at_bind() {
    let host = bound("");
    host.tick();
    let DeviceState::Backoff { reason, .. } = host.device("csi0").unwrap().state else {
        panic!("the driver bound to a stream it cannot use");
    };
    assert!(
        reason.contains(&format!("{:?}", Status::NOT_SUPPORTED)),
        "{reason}"
    );
    // Nobody had to attach for the driver to know.
    assert_eq!(host.stream_stats(0, "frames").unwrap().attachments, 0);
    // The same board with the stream as dma-bufs binds; nothing is opened until `start`.
    let host = bound("memory = \"dma-buf\"\n");
    host.tick();
    assert_eq!(host.device("csi0").unwrap().state, DeviceState::Running);
}

#[test]
fn buffers_smaller_than_a_frame_are_refused() {
    // The stream's buffers hold a 64 by 32 frame; the link is set up for 640 by 480.
    let host = host();
    let hw = Hw::default();
    let mut frames = Client::attach(&host);
    let frames_end: StreamHandle =
        lemnos_core::Held::boxed(Box::new(host.stream_producer(0, "frames").unwrap()));
    let mut receiver = Rp1Csi2::with_hardware(Box::new(hw.clone()), Some(frames_end));
    let big = LinkConfig {
        width: 640,
        height: 480,
        ..link()
    };
    assert!(receiver.configure(big, Packing::Packed).unwrap().size > 4096);
    assert_eq!(receiver.start().unwrap_err(), Status::INVALID_ARGUMENT);
    assert_eq!(hw.with(|b| (b.going, b.given)), (false, 0));
    assert!(frames.ring.pop_frame().unwrap().is_none());
}
