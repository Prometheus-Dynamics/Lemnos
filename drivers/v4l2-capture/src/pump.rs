//! The pump: frames from a capture node into a frame stream.

use lemnos_contract_camera_receiver::Counters;
use lemnos_core::{Result, Status};
use lemnos_hal::blocks::{BufferHandle, BufferKind, StreamFrame};
use lemnos_hal::{ErrorKind, StreamEndpoint};
use lemnos_linux_sys::dma_buf;

use crate::hw::{Frame, Granted, Hardware, MAX_PLANES, Wanted};

/// The most slots a node is asked for when it fills the stream's buffers. A stream with more
/// buffers than this is served by copy.
pub const MAX_SLOTS: u32 = 32;

/// How frames reach the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The node fills the stream's own buffers; nothing is copied.
    Direct,
    /// The node fills its own buffers and each frame is copied into the stream.
    Copy,
}

/// The node's sequence number, widened to 64 bits across its wraps.
#[derive(Debug, Clone, Copy, Default)]
struct Sequence {
    last: Option<u32>,
    wraps: u64,
}

impl Sequence {
    fn widen(&mut self, sequence: u32) -> u64 {
        if self.last.is_some_and(|last| sequence < last) {
            self.wraps = self.wraps.wrapping_add(1);
        }
        self.last = Some(sequence);
        (self.wraps << 32) | u64::from(sequence)
    }
}

/// What a buffer's planes turned out to be, when they were looked for.
#[derive(Clone, Copy)]
enum Found {
    /// Every plane has a handle long enough.
    All,
    /// A plane's dma-buf is shorter than the node writes.
    TooSmall,
    /// Nothing is behind the buffer now.
    Nothing,
}

/// What became of one frame.
enum Delivery {
    Published,
    /// No consumer, or no stream buffer free.
    Dropped,
    /// Flagged bad by the node, or didn't fit.
    Bad,
}

/// Frames from a capture node into a frame stream, one way or the other.
#[derive(Debug)]
pub struct Pump {
    mode: Mode,
    /// Buffers the node was asked for (by copy) or slots (direct).
    buffers: u32,
    counters: Counters,
    sequence: Sequence,
    /// By copy: a stream buffer kept from a frame that didn't fit, for the next one.
    held: Option<u32>,
    /// Direct: the slots the node holds, one bit each.
    queued: u32,
    /// Each memory plane's size as the node chose it: what a stream buffer's plane must hold.
    plane_sizes: [u32; MAX_PLANES],
    /// Memory planes of the node's frames.
    planes: usize,
    /// By copy: each of the node's planes lands at its own plane of the stream's buffer,
    /// rather than all of them end to end.
    by_plane: bool,
    /// Direct: stream buffers too small for a frame, kept here so that they are neither
    /// handed to the node nor shown to the consumer as frames.
    parked: u32,
}

impl Pump {
    /// A pump that has not begun.
    #[must_use]
    pub fn new() -> Self {
        Self {
            mode: Mode::Copy,
            buffers: 0,
            counters: Counters::default(),
            sequence: Sequence::default(),
            held: None,
            queued: 0,
            plane_sizes: [0; MAX_PLANES],
            planes: 0,
            by_plane: false,
            parked: 0,
        }
    }

    /// How frames reach the stream since the last [`begin`](Self::begin).
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Counted since the last [`begin`](Self::begin).
    #[must_use]
    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// Opens the node for `wanted`, checks it kept what was asked, and starts it: filling the
    /// stream's own buffers when `stream` has handles, as many planes as the node's frames
    /// have, and the node takes them; by copy with `buffers` buffers of its own otherwise.
    /// Whatever ran before is stopped first.
    pub fn begin(
        &mut self,
        hw: &mut dyn Hardware,
        stream: &dyn StreamEndpoint<Error = ErrorKind>,
        wanted: &Wanted,
        buffers: u32,
    ) -> Result<()> {
        // Whatever was running ends first; a failure here is not a reason to not try the start.
        let _ = hw.stop();
        let open = |hw: &mut dyn Hardware| -> Result<Granted> {
            let granted = hw.open(wanted)?;
            // A frame in several planes holds what was asked over all of them.
            let held: u64 = granted
                .plane_sizes()
                .iter()
                .map(|size| u64::from(*size))
                .sum();
            let as_asked = granted.fourcc == wanted.fourcc
                && granted.width == wanted.width
                && granted.height == wanted.height
                && granted.stride == wanted.stride
                && (1..=MAX_PLANES).contains(&granted.planes)
                && granted.size == granted.plane_sizes[0]
                && held >= u64::from(wanted.size);
            if !as_asked {
                let _ = hw.stop();
                return Err(Status::NOT_SUPPORTED);
            }
            Ok(granted)
        };
        let mut granted = open(hw)?;
        let slots = stream.buffer_count();
        // The stream's buffers must be the node's planes, one for one.
        let direct = stream.buffer_kind() == BufferKind::Handles
            && slots <= MAX_SLOTS
            && stream.planes() as usize == granted.planes;
        self.mode = Mode::Copy;
        if direct {
            match hw.stream_on_importing(slots) {
                Ok(()) => self.mode = Mode::Direct,
                // The node takes no dma-bufs: by copy, on a node opened afresh.
                Err(Status::NOT_SUPPORTED) => {
                    let _ = hw.stop();
                    granted = open(hw)?;
                }
                Err(status) => return Err(status),
            }
        }
        if self.mode == Mode::Copy {
            hw.stream_on(buffers)?;
        }
        self.buffers = if self.mode == Mode::Direct {
            slots
        } else {
            buffers
        };
        self.counters = Counters::default();
        self.sequence = Sequence::default();
        self.queued = 0;
        self.parked = 0;
        self.planes = granted.planes;
        self.plane_sizes = granted.plane_sizes;
        // A frame of one plane is at least what was asked: a stream buffer must hold that.
        if self.planes == 1 {
            self.plane_sizes[0] = self.plane_sizes[0].max(wanted.size);
        }
        self.by_plane = granted.planes > 1 && stream.planes() as usize == granted.planes;
        Ok(())
    }

    /// Takes every buffer the node has filled, at most one round, and keeps it fed. An error
    /// means the node failed. Allocates nothing.
    pub fn pump(
        &mut self,
        hw: &mut dyn Hardware,
        stream: &mut dyn StreamEndpoint<Error = ErrorKind>,
    ) -> Result<()> {
        match self.mode {
            Mode::Copy => self.pump_copy(hw, stream),
            Mode::Direct => self.pump_direct(hw, stream),
        }
    }

    fn count(&mut self, delivery: &Delivery) {
        let counter = match delivery {
            Delivery::Published => &mut self.counters.frames,
            Delivery::Dropped => &mut self.counters.dropped,
            Delivery::Bad => &mut self.counters.errors,
        };
        *counter = counter.saturating_add(1);
    }

    fn pump_copy(
        &mut self,
        hw: &mut dyn Hardware,
        stream: &mut dyn StreamEndpoint<Error = ErrorKind>,
    ) -> Result<()> {
        for _ in 0..self.buffers {
            let Some(frame) = hw.dequeue()? else {
                return Ok(());
            };
            let delivery = self.copy(hw, stream, &frame);
            self.count(&delivery);
            hw.requeue(frame.index)?;
        }
        Ok(())
    }

    /// Copies one frame into a stream buffer and publishes it.
    fn copy(
        &mut self,
        hw: &mut dyn Hardware,
        stream: &mut dyn StreamEndpoint<Error = ErrorKind>,
        frame: &Frame,
    ) -> Delivery {
        let sequence = self.sequence.widen(frame.sequence);
        if frame.bad {
            return Delivery::Bad;
        }
        let buffer = match self.held.take() {
            Some(buffer) => buffer,
            None => match stream.next_buffer() {
                Ok(Some(buffer)) => buffer,
                Ok(None) | Err(_) => return Delivery::Dropped,
            },
        };
        let mut offset = 0usize;
        for plane in 0..frame.planes {
            let Ok(bytes) = hw.payload(frame, plane) else {
                self.held = Some(buffer);
                return Delivery::Bad;
            };
            // Into the stream's plane of the same number, when its buffers are the node's
            // planes; end to end otherwise.
            if self.by_plane {
                let place = u32::try_from(plane)
                    .ok()
                    .and_then(|plane| stream.plane_offset(plane));
                let Some(place) = place else {
                    self.held = Some(buffer);
                    return Delivery::Bad;
                };
                offset = place;
            }
            match stream.write(buffer, offset, bytes) {
                Ok(true) => offset = offset.saturating_add(bytes.len()),
                // Nobody to land on: hand the buffer back empty, which the stream drops.
                Ok(false) => {
                    let _ = stream.publish(StreamFrame {
                        buffer,
                        bytes: 0,
                        sequence,
                        identity: 0,
                        timestamp_ns: frame.timestamp_ns,
                        end_ns: 0,
                    });
                    return Delivery::Dropped;
                }
                Err(_) => {
                    self.held = Some(buffer);
                    return Delivery::Bad;
                }
            }
        }
        let Ok(bytes) = u32::try_from(offset) else {
            self.held = Some(buffer);
            return Delivery::Bad;
        };
        let published = stream.publish(StreamFrame {
            buffer,
            bytes,
            sequence,
            identity: 0,
            timestamp_ns: frame.timestamp_ns,
            end_ns: 0,
        });
        match published {
            Ok(()) => Delivery::Published,
            Err(_) => {
                self.held = Some(buffer);
                Delivery::Dropped
            }
        }
    }

    /// Gives `buffer` up: published empty, to a consumer that isn't there.
    fn give_up(stream: &mut dyn StreamEndpoint<Error = ErrorKind>, buffer: u32) {
        let _ = stream.publish(StreamFrame {
            buffer,
            bytes: 0,
            sequence: 0,
            identity: 0,
            timestamp_ns: 0,
            end_ns: 0,
        });
    }

    /// Hands stream buffer `buffer` to the node as its slot of the same number. `false` when
    /// nothing is behind the buffer (no consumer, or one that has gone), and the buffer was
    /// given up.
    fn feed(
        &mut self,
        hw: &mut dyn Hardware,
        stream: &mut dyn StreamEndpoint<Error = ErrorKind>,
        buffer: u32,
    ) -> Result<bool> {
        if buffer >= self.buffers {
            // Not a slot the node has: never queued, and given back.
            Self::give_up(stream, buffer);
            return Ok(false);
        }
        // Every plane's handle, each checked: the node writes a whole plane, so the dma-buf's
        // real size, asked of the kernel, must hold it. A stream whose buffers are too small
        // is the board file's mistake, not the node's: counted, and nothing is queued.
        let queued = {
            let mut planes: [Option<BufferHandle<'_>>; MAX_PLANES] = std::array::from_fn(|_| None);
            let mut found = Found::All;
            for (plane, (slot, need)) in planes
                .iter_mut()
                .zip(self.plane_sizes)
                .take(self.planes)
                .enumerate()
            {
                match stream.plane_handle(buffer, plane as u32) {
                    Ok(Some(handle)) => {
                        if !dma_buf::size(handle.fd()).is_ok_and(|size| size >= u64::from(need)) {
                            found = Found::TooSmall;
                            break;
                        }
                        *slot = Some(handle);
                    }
                    Ok(None) | Err(_) => {
                        found = Found::Nothing;
                        break;
                    }
                }
            }
            match (found, planes[0].clone()) {
                (Found::All, Some(first)) => {
                    let mut handles: [BufferHandle<'_>; MAX_PLANES] =
                        std::array::from_fn(|_| first.clone());
                    for (into, from) in handles.iter_mut().zip(planes) {
                        if let Some(handle) = from {
                            *into = handle;
                        }
                    }
                    Ok(hw.queue_handles(buffer, handles.get(..self.planes).unwrap_or(&[])))
                }
                (Found::TooSmall, _) => Err(true),
                _ => Err(false),
            }
        };
        let queued = match queued {
            Ok(queued) => queued,
            Err(true) => {
                self.counters.errors = self.counters.errors.saturating_add(1);
                self.parked |= 1 << buffer;
                return Ok(true);
            }
            Err(false) => {
                Self::give_up(stream, buffer);
                return Ok(false);
            }
        };
        match queued {
            Ok(()) => {
                self.queued |= 1 << buffer;
                Ok(true)
            }
            Err(status) => {
                // The node refused it: the buffer goes back, and the node is counted failed.
                Self::give_up(stream, buffer);
                Err(status)
            }
        }
    }

    fn pump_direct(
        &mut self,
        hw: &mut dyn Hardware,
        stream: &mut dyn StreamEndpoint<Error = ErrorKind>,
    ) -> Result<()> {
        // What the node has filled: each is the stream buffer of its slot's number.
        for _ in 0..self.buffers {
            let Some(frame) = hw.dequeue()? else {
                break;
            };
            let sequence = self.sequence.widen(frame.sequence);
            let bit = 1u32.checked_shl(frame.index).unwrap_or(0);
            if frame.index >= self.buffers || self.queued & bit == 0 {
                // A slot the pump never queued: the node is not doing what it was asked.
                return Err(Status::IO);
            }
            self.queued &= !bit;
            let delivery = if frame.bad {
                // Nothing to show for it: back to the node, or given up if its consumer went.
                self.feed(hw, stream, frame.index)?;
                Delivery::Bad
            } else if !matches!(stream.buffer_current(frame.index), Ok(true)) {
                // Filled for a consumer that has gone.
                Self::give_up(stream, frame.index);
                Delivery::Dropped
            } else {
                let published = stream.publish(StreamFrame {
                    buffer: frame.index,
                    bytes: frame.bytes,
                    sequence,
                    identity: 0,
                    timestamp_ns: frame.timestamp_ns,
                    end_ns: 0,
                });
                match published {
                    Ok(()) => Delivery::Published,
                    Err(_) => {
                        // More bytes than the buffer holds, by the node's count: not shown
                        // to the consumer, and the buffer goes round again.
                        self.feed(hw, stream, frame.index)?;
                        Delivery::Bad
                    }
                }
            };
            self.count(&delivery);
        }
        // Buffers parked under a consumer that has gone are given up, to be looked at afresh.
        if self.parked != 0
            && !matches!(
                stream.buffer_current(self.parked.trailing_zeros()),
                Ok(true)
            )
        {
            while self.parked != 0 {
                let buffer = self.parked.trailing_zeros();
                Self::give_up(stream, buffer);
                self.parked &= !(1 << buffer);
            }
        }
        // What the consumer has given back: to the node, to fill.
        for _ in 0..self.buffers {
            let Ok(Some(buffer)) = stream.next_buffer() else {
                break;
            };
            if !self.feed(hw, stream, buffer)? {
                // Nobody is there: look again at the next tick, not buffer by buffer.
                break;
            }
        }
        Ok(())
    }
}

impl Default for Pump {
    fn default() -> Self {
        Self::new()
    }
}

/// The direct-import path through `queue_handles`, end to end over real memfds.
#[cfg(test)]
mod handle_tests {
    use std::fs;
    use std::os::fd::OwnedFd;

    use lemnos_core::{Result, Status};
    use lemnos_hal::blocks::BufferHandle;
    use lemnos_hal::mock::MockStream;
    use lemnos_linux_sys::dma_buf::sealed_memfd;
    use lemnos_v4l2::FourCc;

    use super::{Mode, Pump};
    use crate::hw::{Frame, Granted, Graph, Hardware, MAX_PLANES, Wanted};

    /// How many descriptors this process holds on the memfds with these inodes. Counts only the
    /// stream's own memfds, so another test running alongside does not change the answer.
    fn held_of(inodes: &[u64]) -> usize {
        use std::os::unix::fs::MetadataExt;
        fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(|entry| fs::metadata(entry.unwrap().path()).ok())
            .filter(|meta| inodes.contains(&meta.ino()))
            .count()
    }

    /// A node that imports dma-bufs: a queued slot comes back filled on the next dequeue. It keeps
    /// no handle; the `BufferHandle`s it is given are used within the call and go.
    #[derive(Default)]
    struct Importer {
        queued: Vec<u32>,
        sequence: u32,
        imports: u32,
        dequeued: u32,
    }

    impl Hardware for Importer {
        fn open(&mut self, wanted: &Wanted) -> Result<Granted> {
            let mut plane_sizes = [0u32; MAX_PLANES];
            plane_sizes[0] = wanted.size;
            Ok(Granted {
                fourcc: wanted.fourcc,
                width: wanted.width,
                height: wanted.height,
                stride: wanted.stride,
                size: wanted.size,
                planes: 1,
                plane_sizes,
            })
        }
        fn stream_on(&mut self, _buffers: u32) -> Result<()> {
            Err(Status::NOT_SUPPORTED)
        }
        fn stream_on_importing(&mut self, _slots: u32) -> Result<()> {
            Ok(())
        }
        fn queue_handles(&mut self, index: u32, planes: &[BufferHandle<'_>]) -> Result<()> {
            assert_eq!(planes.len(), 1);
            self.imports += 1;
            self.queued.push(index);
            Ok(())
        }
        fn stop(&mut self) -> Result<()> {
            self.queued.clear();
            Ok(())
        }
        fn dequeue(&mut self) -> Result<Option<Frame>> {
            if self.queued.is_empty() {
                return Ok(None);
            }
            let index = self.queued.remove(0);
            self.dequeued += 1;
            self.sequence += 1;
            Ok(Some(Frame {
                index,
                sequence: self.sequence,
                timestamp_ns: 500 + u64::from(self.sequence),
                bad: false,
                planes: 1,
                bytes: 16,
            }))
        }
        fn payload(&self, _frame: &Frame, _plane: usize) -> Result<&[u8]> {
            Err(Status::NOT_SUPPORTED)
        }
        fn requeue(&mut self, index: u32) -> Result<()> {
            self.queued.push(index);
            Ok(())
        }
        fn graph(&mut self) -> Result<Graph> {
            Err(Status::NOT_SUPPORTED)
        }
        fn inputs(&mut self) -> Result<Vec<lemnos_v4l2::v4l2::InputInfo>> {
            Err(Status::NOT_SUPPORTED)
        }
        fn input(&mut self) -> Result<u32> {
            Err(Status::NOT_SUPPORTED)
        }
    }

    fn wanted() -> Wanted {
        Wanted {
            input: None,
            fourcc: FourCc::new(b"NV12"),
            width: 64,
            height: 32,
            stride: 64,
            size: 3072,
            mode: 0,
        }
    }

    /// A control, not a regression test. It passes at origin too, and that is the point: the
    /// driver's handles are borrowed from the stream's descriptors for one `queue_handles` call and
    /// never kept, so once the stream is gone no fd of it is open anywhere in the driver. A driver
    /// that stored a handle would keep the stream's memfds open after the stream went, and this
    /// count would not return to its before-value. The mock stands in for the kernel's queue; it
    /// has real memfds, so the descriptors are the kernel's own kind.
    #[test]
    fn a_cycle_through_the_handles_leaves_no_fd_of_the_stream_open_in_the_driver() {
        let fds: Vec<OwnedFd> = (0..4)
            .map(|_| sealed_memfd(c"v4l2-control", 4096).unwrap())
            .collect();
        let inodes: Vec<u64> = fds
            .iter()
            .map(|fd| {
                use std::os::fd::AsRawFd;
                use std::os::unix::fs::MetadataExt;
                fs::metadata(format!("/proc/self/fd/{}", fd.as_raw_fd()))
                    .unwrap()
                    .ino()
            })
            .collect();
        let mut stream = MockStream::with_handles(fds);
        let consumer = stream.clone();
        let mut node = Importer::default();
        let mut pump = Pump::new();
        pump.begin(&mut node, &stream, &wanted(), 4).unwrap();
        assert_eq!(pump.mode(), Mode::Direct);
        pump.pump(&mut node, &mut stream).unwrap();
        assert!(node.imports >= 1, "a buffer was queued through its handle");
        // The stream and its consumer go; the driver (the pump and the node) stays.
        drop(consumer);
        drop(stream);
        assert_eq!(
            held_of(&inodes),
            0,
            "the driver still holds an fd of the stream after it went"
        );
        drop((pump, node));
    }
}
