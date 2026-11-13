//! Outside readers on a stream's own buffers. A stream's consumer asks the daemon for a reader ticket;
//! the descriptors it gets go to a third process, which reads the ring and the buffers read-only, marks
//! through its own page only, and is cut off by the limits it was granted.
//!
//! Each reader is this test binary run again: `reader_role` runs only when `LEMNOS_READER` is set, and
//! does one scenario. The parent passes the ticket's descriptors over a socket pair that is the child's
//! stdin, and reads what the child prints. Every child is one this test started, by the PID `Child`
//! recorded; nothing is stopped by name.

// The reader child is this binary run again: it reports its results on stdout, and its diagnostics on stderr.
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod common;

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Running, start_with, start_with_buffers};
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::StreamFrame;
use lemnos_host::stream::ProducerEndpoint;
use lemnos_linux_sys::scm::{recv_with_fds, send_with_fds};
use lemnos_wire::ring::{Gone, Layout, TicketReader, ticket::fds};
use lemnos_wire::{Frame, VERSION, append, decode};
use lemnosd::DmaHeaps;
use shm_ring::{FieldId, MemMut, Reader, WriteMap};
use shm_ring_buffers::{BufferReader, NextBatch, PoolView, page::cut};
use shm_ring_holds::Marks;

const DEVICE: u32 = 0;
const NAME: &str = "frames";
const BUFFER_BYTES: u32 = 4096;
/// Eight buffers: the producer keeps two free, the consumer one, and a reader's default grant of two
/// fits in what is left.
const BUFFERS: u32 = 8;
const PAYLOAD: usize = 16;

/// A shared-memory board: `readers` slots, and the most a reader may be granted, in buffers and in
/// milliseconds.
fn board(readers: u32, held_max: u32, hold_ms_max: u32) -> String {
    format!(
        "[[devices]]\nid = \"s\"\ndriver = \"fake-counter\"\n\
         [devices.streams.frames]\nbuffers = {BUFFERS}\nbuffer_bytes = {BUFFER_BYTES}\n\
         readers = {readers}\nreader_held_max = {held_max}\nreader_hold_ms_max = {hold_ms_max}\n"
    )
}

/// A dma-buf board: the same stream, its buffers from the system dma-heap, with readers and with or
/// without the import right on the stream.
fn dma_board(readers: u32, reader_import: bool) -> String {
    format!(
        "[[devices]]\nid = \"s\"\ndriver = \"fake-counter\"\n\
         [devices.streams.frames]\nbuffers = {BUFFERS}\nbuffer_bytes = {BUFFER_BYTES}\n\
         memory = \"dma-buf\"\nheap = \"system\"\nreaders = {readers}\n\
         reader_held_max = 4\nreader_hold_ms_max = 10000\nreader_import = {reader_import}\n"
    )
}

/// The protocol, spoken by hand: send a frame, and read what comes back with its descriptors.
struct Wire {
    socket: UnixStream,
    buffer: Vec<u8>,
    filled: usize,
    fds: Vec<OwnedFd>,
}

impl Wire {
    fn connect(path: &Path) -> Self {
        let socket = UnixStream::connect(path).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut wire = Self {
            socket,
            buffer: vec![0; 1 << 16],
            filled: 0,
            fds: Vec::new(),
        };
        wire.send(&Frame::Hello {
            core: lemnos_wire::CORE_HASH,
            lemnos: lemnos_wire::LemnosName(lemnos_wire::LEMNOS),
            version: VERSION,
            name: "stream-readers",
            events: false,
            parts: lemnos_core::Tail(&lemnos_wire::PART_BYTES),
        });
        wire.next(|frame| matches!(frame, Frame::Welcome { .. }).then_some(()));
        wire
    }

    fn send(&mut self, frame: &Frame<'_>) {
        let mut out = Vec::new();
        append(frame, &mut out).unwrap();
        self.socket.write_all(&out).unwrap();
    }

    fn next<R>(&mut self, mut want: impl FnMut(Frame<'_>) -> Option<R>) -> R {
        loop {
            let mut used = 0;
            let mut found = None;
            while let Ok(Some((frame, n))) = decode(&self.buffer[used..self.filled]) {
                used += n;
                found = want(frame);
                if found.is_some() {
                    break;
                }
            }
            self.buffer.copy_within(used..self.filled, 0);
            self.filled -= used;
            if let Some(found) = found {
                return found;
            }
            let n = recv_with_fds(
                self.socket.as_fd(),
                &mut self.buffer[self.filled..],
                &mut self.fds,
            )
            .expect("the socket");
            assert!(n > 0, "the service closed the connection");
            self.filled += n;
        }
    }

    /// Opens a shared-memory stream as its consumer. The stream stays open while this wire lives.
    fn open(&mut self) {
        self.send(&Frame::StreamOpen {
            id: 1,
            device: DEVICE,
            name: NAME,
        });
        self.next(|frame| matches!(frame, Frame::StreamRings { .. }).then_some(()));
        self.fds.clear();
    }

    /// Opens a stream of dma-bufs as its consumer, which takes one descriptor per buffer.
    fn open_handles(&mut self) {
        self.send(&Frame::StreamOpenHandles {
            id: 1,
            device: DEVICE,
            name: NAME,
        });
        self.next(|frame| match frame {
            Frame::StreamRingHandles { .. } => Some(()),
            Frame::Refusal { status, text, .. } => {
                panic!("the stream was refused: {status} {text}")
            }
            _ => None,
        });
        self.fds.clear();
    }

    /// Asks for a reader slot with the import right. See `ticket_import`.
    fn ticket(&mut self, id: u32, held: u32, hold_ms: u32) -> Result<Granted, (i32, String)> {
        self.ticket_import(id, held, hold_ms, true)
    }

    /// Asks for a reader slot. The descriptors that came with the answer are taken out with it.
    /// `Err` carries the refusal's status and its text.
    fn ticket_import(
        &mut self,
        id: u32,
        held: u32,
        hold_ms: u32,
        import: bool,
    ) -> Result<Granted, (i32, String)> {
        self.fds.clear();
        self.send(&Frame::StreamTicket {
            id,
            device: DEVICE,
            name: NAME,
            held,
            hold_ms,
            import,
        });
        let answer = self.next(|frame| match frame {
            Frame::StreamTicketRings {
                slot,
                buffers,
                buffer_bytes,
                frame_slots,
                job_slots,
                job_bytes,
                held,
                hold_ms,
                tail_bytes: _,
                rights,
                descriptors,
                plane_bytes,
                ..
            } => {
                let planes: Vec<u32> = plane_bytes
                    .chunks(4)
                    .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
                    .collect();
                Some(Ok((
                    slot,
                    (buffers, buffer_bytes, frame_slots, job_slots, job_bytes),
                    held,
                    hold_ms,
                    (rights, descriptors),
                    planes,
                )))
            }
            Frame::Refusal { status, text, .. } => Some(Err((status, text.to_string()))),
            _ => None,
        });
        let (slot, shape, held, hold_ms, (rights, descriptors), planes) = answer?;
        assert_eq!(
            self.fds.len(),
            usize::from(descriptors),
            "the descriptors that came with the ticket"
        );
        let (buffers, buffer_bytes, frame_slots, job_slots, job_bytes) = shape;
        Ok(Granted {
            slot,
            layout: Layout {
                buffers,
                buffer_bytes,
                frame_slots,
                job_slots,
                job_bytes,
            },
            planes,
            held,
            hold_ms,
            shared: descriptors as usize == fds::BUFFERS + 1,
            rights,
            fds: std::mem::take(&mut self.fds),
        })
    }

    fn unticket(&mut self, id: u32, slot: u32) -> Result<(), i32> {
        self.send(&Frame::StreamUnticket {
            id,
            device: DEVICE,
            name: NAME,
            slot,
        });
        self.next(|frame| match frame {
            Frame::Reply { .. } => Some(Ok(())),
            Frame::Refusal { status, .. } => Some(Err(status)),
            _ => None,
        })
    }

    /// The host's account of reader `slot`: (live, reason, cut_at_ns, cuts).
    fn ticket_state(&mut self, id: u32, slot: u32) -> (bool, u64, u64, u64) {
        self.send(&Frame::StreamTicketState {
            id,
            device: DEVICE,
            name: NAME,
            slot,
        });
        self.next(|frame| match frame {
            Frame::StreamTicketInfo {
                live,
                reason,
                cut_at_ns,
                cuts,
                ..
            } => Some((live, reason, cut_at_ns, cuts)),
            _ => None,
        })
    }

    fn close(&mut self, id: u32) {
        self.send(&Frame::StreamClose {
            id,
            device: DEVICE,
            name: NAME,
        });
        self.next(|frame| matches!(frame, Frame::Reply { .. }).then_some(()));
    }
}

/// A reader slot as the daemon granted it, with its descriptors in the order of [`fds`].
struct Granted {
    slot: u32,
    layout: Layout,
    planes: Vec<u32>,
    held: u32,
    hold_ms: u32,
    shared: bool,
    /// What the reader was granted: `lemnos_wire::ring::ticket::rights` bits.
    rights: u8,
    fds: Vec<OwnedFd>,
}

/// The pattern a frame's bytes carry, so a reader can tell which frame it has.
fn pattern(seq: u64) -> [u8; PAYLOAD] {
    [seq as u8; PAYLOAD]
}

/// Publishes frame `seq` into a free buffer, with its pattern, if the producer can get one.
fn publish(producer: &mut ProducerEndpoint, seq: u64) -> Option<u32> {
    let buffer = producer.next_buffer().expect("next buffer")?;
    assert_eq!(producer.write(buffer, 0, &pattern(seq)), Ok(true));
    producer
        .publish(StreamFrame {
            buffer,
            bytes: PAYLOAD as u32,
            sequence: seq,
            identity: seq * 10,
            timestamp_ns: seq,
        })
        .expect("publish");
    Some(buffer)
}

/// The text after `READER ` in a line the child printed. The test harness writes `test reader_role ... `
/// without a newline before the child's first line, so the marker is not always at the start.
fn reader_line(text: &str) -> Option<String> {
    let at = text.find("READER ")?;
    Some(text[at + "READER ".len()..].trim_end().to_string())
}

/// The child, a reader in another process, and what it prints.
struct ReaderProcess {
    child: Child,
    /// The parent's end of the socket that is the child's stdin: it sends the child what it waits
    /// for, and the child's descriptors went over it first.
    socket: UnixStream,
    out: BufReader<ChildStdout>,
}

impl ReaderProcess {
    /// Starts `scenario` as a reader holding `granted`: the descriptors go over the socket, and the
    /// service's own copies are closed here, as a service must, so the liveness pipe sees only the
    /// reader.
    fn start(scenario: &str, granted: Granted, extra: &[(&str, String)]) -> Self {
        let (parent, child_end) = UnixStream::pair().unwrap();
        let layout = format!(
            "{},{},{},{},{}",
            granted.layout.buffers,
            granted.layout.buffer_bytes,
            granted.layout.frame_slots,
            granted.layout.job_slots,
            granted.layout.job_bytes
        );
        let planes = granted
            .planes
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["reader_role", "--exact", "--nocapture", "--test-threads=1"])
            .env("LEMNOS_READER", scenario)
            .env("LEMNOS_READER_LAYOUT", layout)
            .env("LEMNOS_READER_PLANES", planes)
            .env("LEMNOS_READER_SHARED", granted.shared.to_string())
            .env("LEMNOS_READER_SLOT", granted.slot.to_string())
            .env("LEMNOS_READER_HELD", granted.held.to_string())
            .stdin(Stdio::from(OwnedFd::from(child_end)))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for (key, value) in extra {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("the reader process");
        {
            let fds: Vec<BorrowedFd<'_>> = granted.fds.iter().map(AsFd::as_fd).collect();
            let sent = send_with_fds(parent.as_fd(), b"ticket", &fds).expect("the descriptors");
            assert_eq!(sent, 6);
        }
        // `granted.fds` closes here: the service's copies, the liveness pipe's write end among them.
        drop(granted.fds);
        let out = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            socket: parent,
            out,
        }
    }

    /// The next line the child printed, without its `READER ` prefix. Panics at the child's end.
    fn line(&mut self) -> String {
        loop {
            let mut text = String::new();
            let read = self.out.read_line(&mut text).expect("the reader's output");
            assert!(read > 0, "the reader process ended before it said so");
            if let Some(rest) = reader_line(&text) {
                return rest;
            }
        }
    }

    /// The child's remaining lines, and whether it exited cleanly.
    fn finish(mut self) -> (Vec<String>, bool) {
        let mut lines = Vec::new();
        let mut text = String::new();
        loop {
            text.clear();
            match self.out.read_line(&mut text) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if let Some(rest) = reader_line(&text) {
                        lines.push(rest);
                    }
                }
            }
        }
        let status = self.child.wait().expect("the reader's status");
        (lines, status.success())
    }

    /// Tells the child to go on, over the socket it waits on.
    fn go(&mut self) {
        self.socket.write_all(b"go").unwrap();
    }
}

impl Drop for ReaderProcess {
    fn drop(&mut self) {
        // Reaps only the child this test started.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The stream's consumer: the test's own wire connection, open for as long as the test runs, and the
/// daemon in this process.
struct Stream {
    running: Running,
    wire: Wire,
}

impl Stream {
    fn start(board_text: &str) -> Self {
        let running = start_with(board_text);
        let mut wire = Wire::connect(&running.socket);
        wire.open();
        Self { running, wire }
    }

    fn producer(&self) -> ProducerEndpoint {
        self.running
            .service
            .as_ref()
            .unwrap()
            .host()
            .stream_producer(DEVICE, NAME)
            .unwrap()
    }

    fn stats(&self) -> lemnos_host::stream::StreamStats {
        self.running
            .service
            .as_ref()
            .unwrap()
            .host()
            .stream_stats(DEVICE, NAME)
            .unwrap()
    }
}

/// Publishes frames until the producer hands out `wanted`: the buffer a reader holds, once it is
/// free. Returns how long that took, or `None` if it did not happen within `within`.
fn until_reused(
    producer: &mut ProducerEndpoint,
    wanted: u32,
    within: Duration,
) -> Option<Duration> {
    let start = Instant::now();
    let mut seq = 1000;
    while start.elapsed() < within {
        seq += 1;
        if let Some(buffer) = publish(producer, seq)
            && buffer == wanted
        {
            return Some(start.elapsed());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

fn held_buffer(line: &str) -> u32 {
    line.strip_prefix("holding ")
        .unwrap_or_else(|| panic!("the reader holds a buffer: {line}"))
        .parse()
        .unwrap()
}

// ------------------------------------------------------------------------------------------------
// The parent's tests.
// ------------------------------------------------------------------------------------------------

#[test]
fn a_third_process_reads_the_streams_frames_through_a_ticket() {
    let mut stream = Stream::start(&board(2, 4, 10_000));
    let granted = stream.wire.ticket(2, 0, 0).expect("a ticket");
    assert_eq!(
        granted.slot, 1,
        "the first reader slot after the consumer's"
    );
    assert_eq!(
        (granted.held, granted.hold_ms),
        (2, 500),
        "the defaults, when nothing is asked"
    );
    let reader = ReaderProcess::start(
        "read",
        granted,
        &[("LEMNOS_READER_FRAMES", "3".to_string())],
    );
    let mut producer = stream.producer();
    for seq in 1..=40 {
        let _ = publish(&mut producer, seq);
        std::thread::sleep(Duration::from_millis(5));
    }
    let (lines, ok) = reader.finish();
    assert!(ok, "the reader exited cleanly: {lines:?}");
    let frames: Vec<&String> = lines.iter().filter(|l| l.starts_with("frame ")).collect();
    assert!(frames.len() >= 3, "three frames arrived: {lines:?}");
    assert!(
        frames.iter().all(|l| l.contains(" ok capture ")),
        "every frame's bytes were its own, and validated: {lines:?}"
    );
    assert!(
        frames
            .iter()
            .all(|l| l.contains(" capture ") && !l.contains(" capture 0")),
        "every frame carries its capture time: {lines:?}"
    );
    assert!(lines.iter().any(|l| l == "done"), "{lines:?}");
    assert_eq!(
        stream.stats().faults,
        0,
        "the consumer is not faulted by a reader"
    );
}

#[test]
fn a_reader_cannot_write_the_ring_the_pool_or_the_buffers_by_any_route() {
    let mut stream = Stream::start(&board(1, 4, 10_000));
    let granted = stream.wire.ticket(2, 0, 0).expect("a ticket");
    let (lines, ok) = ReaderProcess::start("attack", granted, &[]).finish();
    assert!(ok, "the reader exited cleanly: {lines:?}");
    let line = |key: &str| {
        lines
            .iter()
            .find(|l| l.starts_with(&format!("attack {key} ")))
            .unwrap_or_else(|| panic!("no result for {key}: {lines:?}"))
            .clone()
    };
    assert!(
        line("ring map").contains("refused EACCES"),
        "{}",
        line("ring map")
    );
    assert!(
        line("ring reopen").contains("refused EACCES"),
        "{}",
        line("ring reopen")
    );
    // The owner's mode given back, the seal still refuses every write.
    for key in [
        "ring write",
        "ring map rw",
        "page write",
        "page map rw",
        "buffer write",
    ] {
        assert!(line(key).contains("EPERM"), "{key}: {}", line(key));
    }
    assert!(
        line("marks write").contains("yes"),
        "{}",
        line("marks write")
    );
}

#[test]
fn a_reader_of_dma_bufs_cannot_map_them_writable_and_reads_frames_through_its_ticket() {
    if std::fs::File::open("/dev/dma_heap/system").is_err() {
        eprintln!("skipped: /dev/dma_heap/system can not be opened here");
        return;
    }
    let running = start_with_buffers(&dma_board(1, true), Arc::new(DmaHeaps::default()));
    let mut wire = Wire::connect(&running.socket);
    wire.open_handles();
    let granted = wire.ticket(2, 0, 0).expect("a ticket on dma-bufs");
    assert!(
        !granted.shared,
        "a ticket on dma-bufs has one descriptor per buffer"
    );
    assert_eq!(granted.rights, 3, "read and import, as the stream allows");
    let mut reader = ReaderProcess::start("attack-dma", granted, &[]);
    let (lines, ok) = {
        let mut lines = Vec::new();
        loop {
            let line = reader.line();
            let done = line.starts_with("attack marks");
            lines.push(line);
            if done {
                break;
            }
        }
        (lines, true)
    };
    let _ = reader.child.kill();
    assert!(ok);
    let line = |key: &str| {
        lines
            .iter()
            .find(|l| l.starts_with(&format!("attack {key} ")))
            .unwrap_or_else(|| panic!("no result for {key}: {lines:?}"))
            .clone()
    };
    assert!(
        line("buffer map").contains("refused EACCES"),
        "{}",
        line("buffer map")
    );
    assert!(
        line("buffer reopen").contains("refused"),
        "{}",
        line("buffer reopen")
    );
    assert!(
        line("ring reopen").contains("refused EACCES"),
        "{}",
        line("ring reopen")
    );
}

#[test]
fn ticket_slots_are_partitioned_and_run_out_with_busy_until_a_reader_is_gone() {
    let mut stream = Stream::start(&board(1, 4, 10_000));
    let first = stream.wire.ticket(2, 0, 0).expect("the one slot");
    let marks_first = first.fds[fds::MARKS].try_clone().unwrap();
    let liveness_first = first.fds[fds::LIVENESS].try_clone().unwrap();
    // Another connection has no consumer's standing on the stream.
    let mut other = Wire::connect(&stream.running.socket);
    let (status, text) = other.ticket(3, 0, 0).err().expect("refused");
    assert_eq!(status, lemnos_core::Status::NOT_PERMITTED.0, "{text}");
    // Every slot is taken: a named refusal, naming the stream and the limit.
    let (status, text) = stream.wire.ticket(4, 0, 0).err().expect("refused");
    assert_eq!(status, lemnos_core::Status::BUSY.0);
    assert!(
        text.contains("frames") && text.contains("readers = 1"),
        "{text}"
    );
    // The reader dies: its liveness pipe hangs up and its slot comes back.
    let mut reader = ReaderProcess::start("sleep", first, &[]);
    assert_eq!(reader.line(), "sleeping");
    let _ = reader.child.kill();
    let _ = reader.child.wait();
    drop(liveness_first);
    let started = Instant::now();
    let again = loop {
        match stream.wire.ticket(5, 0, 0) {
            Ok(granted) => break granted,
            Err(_) if started.elapsed() < Duration::from_secs(3) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err((status, text)) => panic!("the slot never came back: {status} {text}"),
        }
    };
    assert_eq!(again.slot, 1);
    let inode = |fd: &OwnedFd| {
        std::fs::File::from(fd.try_clone().unwrap())
            .metadata()
            .unwrap()
            .ino()
    };
    assert_ne!(inode(&marks_first), inode(&again.fds[fds::MARKS]));
}

#[test]
fn the_grant_is_bounded_and_a_grant_that_would_starve_the_producer_is_refused_by_name() {
    // Eight buffers: one for the consumer, two kept for the producer, five to share among readers.
    let mut stream = Stream::start(&board(3, 3, 10_000));
    // Asking for more than the stream's maximum (3) is clamped, not refused.
    let clamped = stream.wire.ticket(2, 9, 0).expect("a grant");
    assert_eq!(clamped.held, 3, "clamped to the stream's maximum");
    // Granted so far: 3. Left after the consumer's one: 8 - 3 - 1 = 4, so another 3 would leave 1 for
    // the producer, which needs 2. The refusal says what was asked and what is left.
    let (status, text) = stream.wire.ticket(3, 3, 0).err().expect("refused");
    assert_eq!(status, lemnos_core::Status::BUSY.0);
    assert!(
        text.contains("frames") && text.contains("asked for 3") && text.contains("4 of 8"),
        "the refusal names the stream, what was asked and what is left: {text}"
    );
}

#[test]
fn grants_share_the_buffers_the_producer_does_not_need() {
    let mut stream = Stream::start(&board(3, 6, 10_000));
    assert_eq!(stream.wire.ticket(2, 2, 0).expect("two").held, 2);
    assert_eq!(stream.wire.ticket(3, 3, 0).expect("three more").held, 3);
    // 8 - 5 - 1 = 2 left, and the producer needs 2: nothing more is granted.
    let (status, text) = stream.wire.ticket(4, 1, 0).err().expect("refused");
    assert_eq!(status, lemnos_core::Status::BUSY.0);
    assert!(
        text.contains("asked for 1") && text.contains("of 8 are left"),
        "{text}"
    );
}

#[test]
fn a_reader_holding_more_than_its_grant_is_cut_off_and_the_producer_goes_on() {
    let mut stream = Stream::start(&board(1, 4, 10_000));
    let granted = stream.wire.ticket(2, 2, 0).expect("a ticket");
    // This reader has no limit of its own on its marks: it marks every buffer it reads.
    let reader = ReaderProcess::start("cheat", granted, &[]);
    let mut producer = stream.producer();
    // The child says nothing until it is cut off, so the producer runs on without waiting for it.
    let start = Instant::now();
    let mut seq = 0;
    while start.elapsed() < Duration::from_millis(900) {
        seq += 1;
        let _ = publish(&mut producer, seq);
        std::thread::sleep(Duration::from_millis(10));
    }
    let (lines, _) = reader.finish();
    let line = lines
        .iter()
        .find(|l| l.starts_with("gone"))
        .cloned()
        .unwrap_or_else(|| panic!("the reader was never cut off: {lines:?}"));
    assert_eq!(line, format!("gone cut-off {}", cut::HELD), "{lines:?}");
    for more in 0..8 {
        assert!(
            publish(&mut producer, 100 + more).is_some(),
            "a buffer is free once the reader is cut off"
        );
    }
    assert_eq!(stream.stats().faults, 0);
    assert!(stream.stats().attached, "the consumer is still attached");
}

#[test]
fn a_reader_holding_one_buffer_past_its_grant_is_cut_off_for_holding_too_long() {
    let mut stream = Stream::start(&board(1, 4, 300));
    let granted = stream.wire.ticket(2, 2, 300).expect("a ticket");
    assert_eq!(granted.hold_ms, 300);
    let mut reader = ReaderProcess::start("hold-long", granted, &[]);
    let mut producer = stream.producer();
    for seq in 1..=2 {
        publish(&mut producer, seq).expect("a buffer");
    }
    assert_eq!(reader.line(), "holding");
    for seq in 3..=10 {
        let _ = publish(&mut producer, seq);
        std::thread::sleep(Duration::from_millis(20));
    }
    let (lines, _) = reader.finish();
    assert!(
        lines
            .iter()
            .any(|l| l == &format!("validate cut-off {}", cut::HOLD_TIME)),
        "validation after the hold said it was cut off for holding too long: {lines:?}"
    );
    assert_eq!(stream.stats().faults, 0);
}

#[test]
fn a_dead_readers_holds_are_released_within_a_bound_and_the_producer_reuses_its_buffer() {
    // A grant of ten seconds: only the liveness pipe can free the buffer within the bound below.
    let mut stream = Stream::start(&board(1, 4, 10_000));
    let granted = stream.wire.ticket(2, 2, 10_000).expect("a ticket");
    let mut reader = ReaderProcess::start("dead", granted, &[]);
    let mut producer = stream.producer();
    for seq in 1..=2 {
        publish(&mut producer, seq).expect("a buffer");
    }
    let held = held_buffer(&reader.line());
    std::thread::sleep(Duration::from_millis(50));
    let killed = Instant::now();
    let _ = reader.child.kill();
    let _ = reader.child.wait();
    let within = until_reused(&mut producer, held, Duration::from_secs(5))
        .expect("the producer got the dead reader's buffer back");
    let bound = Duration::from_secs(2);
    assert!(
        killed.elapsed() <= bound && within <= bound,
        "released in {within:?}, bound {bound:?}"
    );
}

#[test]
fn a_reader_cut_off_while_it_holds_a_frame_finds_the_buffer_recycled_when_it_validates() {
    let mut stream = Stream::start(&board(1, 4, 10_000));
    let granted = stream.wire.ticket(2, 2, 10_000).expect("a ticket");
    let slot = granted.slot;
    let mut reader = ReaderProcess::start("recycle", granted, &[]);
    let mut producer = stream.producer();
    for seq in 1..=2 {
        publish(&mut producer, seq).expect("a buffer");
    }
    let held = held_buffer(&reader.line());
    stream.wire.unticket(6, slot).expect("unticket");
    until_reused(&mut producer, held, Duration::from_secs(5))
        .expect("the producer reused the cut-off reader's buffer");
    reader.go();
    let (lines, _) = reader.finish();
    let verdict = lines
        .iter()
        .find(|l| l.starts_with("validate "))
        .unwrap_or_else(|| panic!("no validation: {lines:?}"))
        .clone();
    assert!(
        verdict == "validate recycled" || verdict == format!("validate cut-off {}", cut::RELEASED),
        "the reader must not take the recycled bytes: {verdict}"
    );
}

#[test]
fn the_service_can_ask_why_a_reader_was_cut_off_and_when() {
    let mut stream = Stream::start(&board(1, 4, 10_000));
    let granted = stream.wire.ticket(2, 2, 10_000).expect("a ticket");
    let slot = granted.slot;
    let mut reader = ReaderProcess::start("sleep", granted, &[]);
    assert_eq!(reader.line(), "sleeping");
    let (live, reason, _, cuts) = stream.wire.ticket_state(7, slot);
    assert!(
        live && reason == 0 && cuts == 0,
        "a live reader, not cut off"
    );
    stream.wire.unticket(8, slot).expect("unticket");
    let started = Instant::now();
    let (live, reason, cut_at, cuts) = loop {
        let state = stream.wire.ticket_state(9, slot);
        if state.1 != 0 || started.elapsed() > Duration::from_secs(3) {
            break state;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(reason, cut::RELEASED, "the service's release is the reason");
    assert!(cut_at > 0, "and when it was cut off");
    assert_eq!(cuts, 1, "one reader of the stream was cut off");
    assert!(live, "still live: the reader has not gone");
    let _ = reader.child.kill();
    let _ = reader.child.wait();
}

#[test]
fn a_ticket_is_refused_by_name_without_readers_or_without_a_consumer() {
    // A stream whose board gives it no readers: NOT_SUPPORTED, and the text names the stream.
    let mut stream = Stream::start(&board(0, 4, 10_000));
    let (status, text) = stream.wire.ticket(2, 0, 0).err().expect("refused");
    assert_eq!(status, lemnos_core::Status::NOT_SUPPORTED.0, "{text}");
    assert!(text.contains(NAME), "{text}");
    // A stream with readers but no consumer attached to it: NOT_FOUND.
    let running = start_with(&board(1, 4, 10_000));
    let mut wire = Wire::connect(&running.socket);
    let (status, text) = wire.ticket(3, 0, 0).err().expect("refused");
    assert_eq!(status, lemnos_core::Status::NOT_FOUND.0, "{text}");
}

#[test]
fn a_reader_learns_the_stream_ended() {
    let mut stream = Stream::start(&board(1, 4, 10_000));
    let granted = stream.wire.ticket(2, 0, 0).expect("a ticket");
    let mut reader = ReaderProcess::start("ended", granted, &[]);
    assert_eq!(reader.line(), "waiting");
    stream.wire.close(7);
    let (lines, ok) = reader.finish();
    assert!(ok, "{lines:?}");
    assert!(
        lines.iter().any(|l| l == "gone ended 1"),
        "the reader saw the ring end as closed: {lines:?}"
    );
}

/// The import right on a dma-buf stream: refused without it, refused under signed mode without the
/// stream's `reader_import`, granted with it, and granted to any ticket under open packages.
#[test]
fn the_import_right_is_granted_only_as_the_stream_and_the_mode_allow() {
    if std::fs::File::open("/dev/dma_heap/system").is_err() {
        eprintln!("skipped: /dev/dma_heap/system can not be opened here");
        return;
    }
    let heap = || Arc::new(DmaHeaps::default());
    // Without the import right, a dma-buf stream's reader gets no descriptor from the daemon at all.
    let running = start_with_buffers(&dma_board(1, true), heap());
    let mut wire = Wire::connect(&running.socket);
    wire.open_handles();
    let (status, text) = wire.ticket_import(2, 0, 0, false).err().expect("refused");
    assert_eq!(status, lemnos_core::Status::NOT_PERMITTED.0);
    assert!(
        text.contains("import right") && text.contains("copy from the service"),
        "{text}"
    );
    drop(wire);
    drop(running);

    // Under signed mode, with the stream not allowing imports: refused, by name.
    let running = start_with_buffers(&dma_board(1, false), heap());
    let mut wire = Wire::connect(&running.socket);
    wire.open_handles();
    let (status, text) = wire.ticket(2, 0, 0).err().expect("refused");
    assert_eq!(status, lemnos_core::Status::NOT_PERMITTED.0);
    assert!(text.contains("reader_import"), "{text}");
    drop(wire);
    drop(running);

    // Open packages: the mode grants the import right to any ticket, whatever the stream says.
    let running = start_with_buffers(&dma_board(1, false), heap());
    running
        .service
        .as_ref()
        .unwrap()
        .host()
        .set_signed_mode(false);
    let mut wire = Wire::connect(&running.socket);
    wire.open_handles();
    let granted = wire.ticket(2, 0, 0).expect("granted under open packages");
    assert_eq!(granted.rights, 3);
}

// ------------------------------------------------------------------------------------------------
// The reader's role: this test binary, run again as a reader. A no-op unless `LEMNOS_READER` is set.
// ------------------------------------------------------------------------------------------------

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("{key} is set by the parent"))
}

fn say(text: &str) {
    println!("READER {text}");
}

/// Receives the ticket's descriptors from stdin, which is the socket the parent passed them over.
fn take_descriptors() -> Vec<OwnedFd> {
    let stdin = std::io::stdin();
    let mut buf = [0_u8; 64];
    let mut fds = Vec::new();
    let n = recv_with_fds(stdin.as_fd(), &mut buf, &mut fds).expect("the ticket");
    assert_eq!(&buf[..n], b"ticket");
    fds
}

fn wait_go() {
    let stdin = std::io::stdin();
    let mut buf = [0_u8; 8];
    let mut no_fds = Vec::new();
    let n = recv_with_fds(stdin.as_fd(), &mut buf, &mut no_fds).expect("go");
    assert_eq!(&buf[..n], b"go");
}

fn layout_from_env() -> Layout {
    let text = env("LEMNOS_READER_LAYOUT");
    let v: Vec<u32> = text.split(',').map(|s| s.parse().unwrap()).collect();
    Layout {
        buffers: v[0],
        buffer_bytes: v[1],
        frame_slots: v[2],
        job_slots: v[3],
        job_bytes: v[4],
    }
}

fn planes_from_env() -> Vec<u32> {
    let text = env("LEMNOS_READER_PLANES");
    if text.is_empty() {
        Vec::new()
    } else {
        text.split(',').map(|s| s.parse().unwrap()).collect()
    }
}

/// `errno` as the names this test checks.
fn errno(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        Some(1) => "EPERM".to_string(),
        Some(13) => "EACCES".to_string(),
        Some(6) => "ENXIO".to_string(),
        _ => format!("{error}"),
    }
}

/// Opens the descriptor for writing, through `/proc`, and reports what happened.
fn reopen_for_write(fd: &OwnedFd) -> String {
    let path = format!("/proc/self/fd/{}", fd.as_raw_fd());
    match OpenOptions::new().write(true).open(&path) {
        Ok(mut file) => match file.write_all(&[0]) {
            Ok(()) => "wrote".to_string(),
            Err(error) => format!("refused {}", errno(&error)),
        },
        Err(error) => format!("refused {}", errno(&error)),
    }
}

/// Asks the kernel for a writable mapping of a dma-buf and reports the refusal by errno.
fn map_dma_writable(fd: &OwnedFd) -> String {
    match lemnos_linux_sys::mmap::Mapping::new(fd.as_fd(), 4096, 0, true) {
        Ok(_) => "mapped".to_string(),
        Err(error) => format!("refused {}", errno(&error)),
    }
}

/// Makes the mapping a writer would, through `fd`, and reports whether it was refused, by errno.
fn map_writable(fd: &OwnedFd) -> String {
    match fd
        .try_clone()
        .map_err(shm_ring::RingError::from)
        .and_then(WriteMap::new)
    {
        Ok(_) => "mapped".to_string(),
        Err(shm_ring::RingError::Io(error)) => format!("refused {}", errno(&error)),
        Err(error) => format!("refused {error}"),
    }
}

#[test]
fn reader_role() {
    let Ok(scenario) = std::env::var("LEMNOS_READER") else {
        return;
    };
    let layout = layout_from_env();
    let planes = planes_from_env();
    let shared = env("LEMNOS_READER_SHARED") == "true";
    let slot: u32 = env("LEMNOS_READER_SLOT").parse().unwrap();
    let held: u32 = env("LEMNOS_READER_HELD").parse().unwrap();
    let fds_in = take_descriptors();
    eprintln!(
        "reader {scenario}: {} descriptors, layout {layout:?}, planes {planes:?}, shared {shared}, slot {slot}, held {held}",
        fds_in.len()
    );
    match scenario.as_str() {
        "attack" => attack(&fds_in, true),
        "attack-dma" => attack(&fds_in, false),
        "sleep" => {
            say("sleeping");
            std::thread::sleep(Duration::from_secs(3600));
        }
        "read" => {
            let wanted: usize =
                std::env::var("LEMNOS_READER_FRAMES").map_or(3, |v| v.parse().unwrap());
            let mut reader =
                TicketReader::attach(fds_in, layout, &planes, shared, slot, held).expect("attach");
            read_frames(&mut reader, wanted);
        }
        "cheat" => {
            // Marks without a limit of its own: the one thing an honest reader does not do.
            let mut reader = attach_unlimited(fds_in, layout, slot);
            cheat(&mut reader);
        }
        "hold-long" => {
            let mut reader =
                TicketReader::attach(fds_in, layout, &planes, shared, slot, held).expect("attach");
            let buffer = wait_for_frame(&mut reader);
            say("holding");
            std::thread::sleep(Duration::from_millis(1500));
            let verdict = match reader.validate(buffer) {
                Ok(()) => "valid".to_string(),
                Err(Gone::CutOff(reason)) => format!("cut-off {reason}"),
                Err(gone) => format!("{gone}"),
            };
            say(&format!("validate {verdict}"));
        }
        "dead" => {
            let mut reader =
                TicketReader::attach(fds_in, layout, &planes, shared, slot, held).expect("attach");
            let buffer = wait_for_frame(&mut reader);
            say(&format!("holding {buffer}"));
            std::thread::sleep(Duration::from_secs(3600));
        }
        "recycle" => {
            let mut reader =
                TicketReader::attach(fds_in, layout, &planes, shared, slot, held).expect("attach");
            let buffer = wait_for_frame(&mut reader);
            say(&format!("holding {buffer}"));
            wait_go();
            let verdict = match reader.validate(buffer) {
                Ok(()) => "valid".to_string(),
                Err(Gone::Recycled) => "recycled".to_string(),
                Err(Gone::CutOff(reason)) => format!("cut-off {reason}"),
                Err(gone) => format!("{gone}"),
            };
            say(&format!("validate {verdict}"));
        }
        "ended" => {
            let mut reader =
                TicketReader::attach(fds_in, layout, &planes, shared, slot, held).expect("attach");
            say("waiting");
            loop {
                match reader.pop() {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        reader.wait(Duration::from_millis(100)).ok();
                    }
                    Err(Gone::Ended(state)) => {
                        say(&format!("gone ended {state}"));
                        break;
                    }
                    Err(gone) => {
                        say(&format!("gone {gone}"));
                        break;
                    }
                }
            }
        }
        other => panic!("no such scenario: {other}"),
    }
}

/// The first frame a reader gets, as its buffer.
fn wait_for_frame(reader: &mut TicketReader) -> u32 {
    let start = Instant::now();
    loop {
        if let Some(frame) = reader.pop().expect("a frame, not a stop") {
            return frame.buffer;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "no frame came");
        reader.wait(Duration::from_millis(50)).ok();
    }
}

fn read_frames(reader: &mut TicketReader, wanted: usize) {
    let start = Instant::now();
    let mut got = 0;
    while got < wanted && start.elapsed() < Duration::from_secs(10) {
        match reader.pop() {
            Ok(Some(frame)) => {
                let mut bytes = [0_u8; PAYLOAD];
                let read = reader.read(frame.buffer, 0, &mut bytes);
                let valid = reader.validate(frame.buffer).is_ok();
                let own = bytes == pattern(frame.sequence);
                say(&format!(
                    "frame {} {} capture {}",
                    frame.sequence,
                    if read && own && valid { "ok" } else { "bad" },
                    frame.capture_ns
                ));
                reader.release(frame.buffer);
                got += 1;
            }
            Ok(None) => {
                reader.wait(Duration::from_millis(50)).ok();
            }
            Err(gone) => {
                say(&format!("gone {gone}"));
                return;
            }
        }
    }
    say("done");
}

/// A reader with its own marks that has no limit: the honest one is `TicketReader`.
fn attach_unlimited(fds_in: Vec<OwnedFd>, layout: Layout, slot: u32) -> CheatReader {
    let mut fds = fds_in.into_iter();
    let ring = fds.next().unwrap();
    let pool_page = fds.next().unwrap();
    let bell_fd = fds.next().unwrap();
    let bell_page = fds.next().unwrap();
    let marks = fds.next().unwrap();
    let liveness = fds.next().unwrap();
    let buffer = fds.next().unwrap();
    let reader = Reader::attach_from_start(ring).expect("ring");
    let descs = layout.pool_descs(true, &[]);
    let view = PoolView::attach(pool_page, vec![buffer], &descs).expect("pool");
    let marks = Marks::over(
        WriteMap::new(marks).expect("marks"),
        layout.buffers,
        slot,
        0,
    );
    let frames = BufferReader::new(reader, FieldId(0), view, marks);
    let bell = shm_ring_doorbell::Doorbell::attach(bell_fd, bell_page).expect("bell");
    CheatReader {
        frames,
        slot,
        _bell: bell,
        _liveness: liveness,
    }
}

/// The reader for the cheat scenario: it keeps every frame it takes and reports the cut-off.
struct CheatReader {
    frames: BufferReader<shm_ring::ReadMap, Marks<WriteMap>>,
    slot: u32,
    _bell: shm_ring_doorbell::Doorbell,
    _liveness: OwnedFd,
}

fn cheat(reader: &mut CheatReader) {
    let start = Instant::now();
    let mut kept = Vec::new();
    while start.elapsed() < Duration::from_secs(10) {
        let reason = reader.frames.pool().cut_off_reason(reader.slot);
        if reason != 0 {
            say(&format!("gone cut-off {reason}"));
            drop(kept);
            return;
        }
        match reader.frames.next() {
            NextBatch::Batch(batch) => kept.push(batch.held),
            NextBatch::CutOff => {
                let reason = reader.frames.pool().cut_off_reason(reader.slot);
                say(&format!("gone cut-off {reason}"));
                return;
            }
            _ => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    say("gone none");
}

/// The descriptors a reader may touch, and what each route to writing them does. `shared` is a stream of
/// memfds (the ring, the pool page and the buffer are sealed); otherwise the buffer is a dma-buf, which
/// has no seal and no reopen, and its read-only mode is all there is.
fn attack(fds_in: &[OwnedFd], shared: bool) {
    let ring = &fds_in[fds::RING];
    let pool_page = &fds_in[fds::POOL_PAGE];
    let buffer = &fds_in[fds::BUFFERS];
    say(&format!("attack ring map {}", map_writable(ring)));
    say(&format!("attack ring reopen {}", reopen_for_write(ring)));
    say(&format!(
        "attack page reopen {}",
        reopen_for_write(pool_page)
    ));
    say(&format!(
        "attack buffer reopen {}",
        reopen_for_write(buffer)
    ));
    if shared {
        // The owner's mode given back, as a process with the same owner could: the seal must still refuse.
        for fd in [ring, pool_page, buffer] {
            let path = format!("/proc/self/fd/{}", fd.as_raw_fd());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))
                .expect("the owner may change the mode");
        }
        let rw = |fd: &OwnedFd| {
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(format!("/proc/self/fd/{}", fd.as_raw_fd()))
                .expect("opens after the mode is given back")
        };
        let mut ring_rw = rw(ring);
        let write = ring_rw
            .write_all(&[0])
            .map_or_else(|e| errno(&e), |()| "wrote".into());
        say(&format!("attack ring write {write}"));
        let mut page_rw = rw(pool_page);
        let write = page_rw
            .write_all(&[0])
            .map_or_else(|e| errno(&e), |()| "wrote".into());
        say(&format!("attack page write {write}"));
        let mut buffer_rw = rw(buffer);
        let write = buffer_rw
            .write_all(&[0])
            .map_or_else(|e| errno(&e), |()| "wrote".into());
        say(&format!("attack buffer write {write}"));
        let mapped = map_writable(&ring_rw.try_clone().unwrap().into());
        say(&format!("attack ring map rw {mapped}"));
        let page_mapped = map_writable(&page_rw.try_clone().unwrap().into());
        say(&format!("attack page map rw {page_mapped}"));
    } else {
        // A dma-buf is not a sealed memfd, so the kernel's answer to a writable mapping is the test.
        say(&format!("attack buffer map {}", map_dma_writable(buffer)));
    }
    // The marks page is the reader's own and writable.
    let marks = WriteMap::new(fds_in[fds::MARKS].try_clone().unwrap());
    let marks_says = match marks {
        Ok(page) => {
            page.store(1, 1, std::sync::atomic::Ordering::SeqCst);
            "yes".to_string()
        }
        Err(error) => format!("no {error}"),
    };
    say(&format!("attack marks write {marks_says}"));
}
