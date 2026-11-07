//! When a frame's first rows are in memory, from what the stock driver gives: its `FRAME_SYNC`
//! event, a delay from the sensor's line time, and a marker. The row after the slice line starts
//! with a marker in every raw buffer before it is queued; a marker gone at the delay's end means
//! the rows above it have landed, whatever the timing did. One still there is an early fire: the
//! frame runs whole. No thread: the caller's wait wakes for [`LineTimer::due`].

use std::cell::{Cell, RefCell};
use std::time::Duration;

use lemnos_v4l2::Mapping;

use super::LineEvent;

/// Written where the row after the slice line starts. Sensor data equal to it would read as
/// not landed, and the frame would run whole.
const MARKER: [u8; 16] = [
    0x5a, 0xc3, 0x96, 0x3d, 0xe1, 0x7b, 0x18, 0xa4, 0x2f, 0xd0, 0x6e, 0x85, 0xb9, 0x43, 0xf7, 0x0c,
];

/// Upper bounds in µs of [`LineTimerCounts::late`]'s first buckets; the last takes the rest.
pub const LATE_BOUNDS_US: [u64; 5] = [50, 100, 250, 500, 1000];

/// What the timer did: wake-ups that came late are not early fires, and only early fires cost a
/// frame its slice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LineTimerCounts {
    /// `FRAME_SYNC` events read.
    pub starts: u64,
    /// Rows found in memory: line events raised.
    pub fired: u64,
    /// Marker still there at the delay's end: the frame runs whole.
    pub early: u64,
    /// The frame in flight was not the event's (the caller is a frame behind, or the stream
    /// just started): the frame runs whole.
    pub stale: u64,
    /// A frame start arrived with the last one's delay still running.
    pub replaced: u64,
    /// Time from the delay's end to the check, by [`LATE_BOUNDS_US`].
    pub late: [u64; LATE_BOUNDS_US.len() + 1],
    /// The longest of those.
    pub worst_late: Duration,
}

#[derive(Clone, Copy)]
struct Due {
    sequence: u32,
    frame_start: Duration,
    at: Duration,
}

pub(super) struct LineTimer {
    line: u32,
    delay: Duration,
    /// Byte offset of the row in each raw buffer's first mapping.
    offset: usize,
    /// The first mapping of each raw buffer, the row's memory.
    rows: RefCell<Vec<Mapping>>,
    /// The other mappings, kept alive as the caller gave them.
    _planes: Vec<Vec<Mapping>>,
    due: Cell<Option<Due>>,
    /// The sequence of the last frame taken from the front end.
    taken: Cell<Option<u32>>,
    counts: Cell<LineTimerCounts>,
}

impl LineTimer {
    /// A timer for `line` rows over raw buffers mapped writable, `offset` the start of the row
    /// after them. `None` when a buffer is too short for that.
    pub(super) fn new(line: u32, mut maps: Vec<Vec<Mapping>>, offset: usize) -> Option<Self> {
        let end = offset.checked_add(MARKER.len())?;
        let mut rows = Vec::with_capacity(maps.len());
        for planes in &mut maps {
            if planes.first()?.len() < end {
                return None;
            }
            rows.push(planes.remove(0));
        }
        Some(Self::with_rows(line, offset, rows, maps))
    }

    fn with_rows(line: u32, offset: usize, rows: Vec<Mapping>, planes: Vec<Vec<Mapping>>) -> Self {
        Self {
            line,
            delay: Duration::ZERO,
            offset,
            rows: RefCell::new(rows),
            _planes: planes,
            due: Cell::new(None),
            taken: Cell::new(None),
            counts: Cell::default(),
        }
    }

    /// Takes a raw buffer added after the others (its index is the next one), mapped
    /// writable. `false` when it is too short for the row.
    pub(super) fn add_buffer(&mut self, mut planes: Vec<Mapping>) -> bool {
        let Some(end) = self.offset.checked_add(MARKER.len()) else {
            return false;
        };
        if planes.first().is_none_or(|row| row.len() < end) {
            return false;
        }
        self.rows.get_mut().push(planes.remove(0));
        self._planes.push(planes);
        true
    }

    /// The time from a frame's start to the check.
    pub(super) fn set_delay(&mut self, delay: Duration) {
        self.delay = delay;
    }

    /// Buffer `index` is about to be queued.
    pub(super) fn arm(&self, index: u32) {
        if let Some(row) = self.rows.borrow_mut().get_mut(index as usize) {
            // Checked at `new` that the row fits; a refused store writes nothing.
            let _ = row.write_volatile(self.offset, MARKER);
        }
    }

    /// A `FRAME_SYNC` event: frame `sequence` started at `frame_start`.
    pub(super) fn frame_start(&self, sequence: u32, frame_start: Duration) {
        let mut c = self.counts.get();
        c.starts += 1;
        c.replaced += u64::from(self.due.get().is_some());
        self.counts.set(c);
        self.due.set(Some(Due {
            sequence,
            frame_start,
            at: frame_start + self.delay,
        }));
    }

    /// Frame `sequence` left the front end.
    pub(super) fn frame_taken(&self, sequence: u32) {
        self.taken.set(Some(sequence));
    }

    /// When the next check is due (`CLOCK_MONOTONIC`).
    pub(super) fn due(&self) -> Option<Duration> {
        self.due.get().map(|d| d.at)
    }

    /// The check, once due: the line event when buffer `in_flight` is the frame's and has its
    /// rows. `None` otherwise, and the frame runs whole.
    pub(super) fn check(&self, now: Duration, in_flight: Option<u32>) -> Option<LineEvent> {
        let due = self.due.get().filter(|d| now >= d.at)?;
        self.due.set(None);
        let mut c = self.counts.get();
        let late = now - due.at;
        let us = late.as_micros() as u64;
        let bucket = LATE_BOUNDS_US.iter().position(|b| us < *b);
        c.late[bucket.unwrap_or(LATE_BOUNDS_US.len())] += 1;
        c.worst_late = c.worst_late.max(late);
        let next = self.taken.get().map(|t| t.wrapping_add(1));
        let rows = self.rows.borrow();
        let row = in_flight
            .filter(|_| next == Some(due.sequence))
            .and_then(|i| rows.get(i as usize));
        let event = match row {
            None => {
                c.stale += 1;
                None
            }
            Some(r) if r.read_volatile(self.offset) == Some(MARKER) => {
                c.early += 1;
                None
            }
            Some(_) => {
                c.fired += 1;
                Some(LineEvent {
                    sequence: due.sequence,
                    line: self.line,
                    at: now,
                    frame_start: due.frame_start,
                })
            }
        };
        self.counts.set(c);
        event
    }

    pub(super) fn counts(&self) -> LineTimerCounts {
        self.counts.get()
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd;

    use super::*;

    const MS: Duration = Duration::from_millis(1);

    /// A timer over `n` raw buffers of one page each, mapped read-write from a temporary file
    /// that is unlinked at once (the mappings keep it). The row is at `offset` of each buffer.
    fn mapped(n: usize, offset: usize) -> Option<LineTimer> {
        // One file per call: tests run in parallel, and two that map the same
        // shape must not share (and delete) one path.
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let call = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "lemnos-pisp-line-timer-{}-{call}-{n}-{offset}",
            std::process::id()
        ));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.set_len(4096 * n as u64).unwrap();
        let maps = (0..n)
            .map(|i| {
                let map = Mapping::new(file.as_fd(), 4096, 4096 * i as u64, true).unwrap();
                vec![map]
            })
            .collect();
        std::fs::remove_file(path).unwrap();
        LineTimer::new(600, maps, offset)
    }

    /// A timer as the caller sets it up: 5 ms delay, both rows armed, frame 6 taken.
    fn timer(n: usize) -> LineTimer {
        let mut t = mapped(n, 0).unwrap();
        t.set_delay(5 * MS);
        t.arm(0);
        t.arm(1);
        t.frame_taken(6);
        t
    }

    /// What the hardware does: writes `bytes` at the row of raw buffer `i`.
    fn device_writes(t: &LineTimer, i: usize, bytes: [u8; 16]) {
        t.rows.borrow_mut()[i].write_volatile(0, bytes).unwrap();
    }

    #[test]
    fn rows_in_memory_raise_the_event_at_the_delays_end() {
        let t = timer(2);
        t.frame_start(7, 100 * MS);
        assert_eq!(t.due(), Some(105 * MS));
        device_writes(&t, 1, [1; 16]);
        assert_eq!(t.check(104 * MS, Some(1)), None, "not due");
        let ev = t.check(105 * MS, Some(1)).unwrap();
        assert_eq!((ev.sequence, ev.line, ev.frame_start), (7, 600, 100 * MS));
        assert_eq!(t.due(), None);
        assert_eq!(t.counts().fired, 1);
    }

    /// A buffer added while streaming (`add_image_buffers`) is armed and checked like the
    /// first ones, at the next index; one too short for the row is refused.
    #[test]
    fn an_added_buffer_takes_the_next_index() {
        let mut t = timer(2);
        let extra = mapped(1, 0).unwrap().rows.into_inner().remove(0);
        assert!(t.add_buffer(vec![extra]));
        assert!(!t.add_buffer(Vec::new()));
        t.arm(2);
        t.frame_start(7, 100 * MS);
        device_writes(&t, 2, [1; 16]);
        let ev = t.check(105 * MS, Some(2)).unwrap();
        assert_eq!((ev.sequence, ev.line), (7, 600));
    }

    #[test]
    fn a_marker_still_there_runs_the_frame_whole() {
        let t = timer(2);
        t.frame_start(7, 100 * MS);
        assert_eq!(t.check(105 * MS, Some(1)), None);
        let c = t.counts();
        assert_eq!((c.fired, c.early, c.stale), (0, 1, 0));
    }

    #[test]
    fn a_late_wake_up_is_not_an_early_fire() {
        let t = timer(2);
        t.frame_start(7, 100 * MS);
        device_writes(&t, 0, [1; 16]);
        assert!(t.check(107 * MS, Some(0)).is_some());
        let c = t.counts();
        assert_eq!((c.early, c.late[LATE_BOUNDS_US.len()]), (0, 1));
        assert_eq!(c.worst_late, 2 * MS);
    }

    #[test]
    fn a_frame_not_in_flight_runs_whole() {
        let t = timer(2);
        // The caller still holds frame 7 in the queue: the buffer in front is not frame 8's,
        // and its marker is long gone.
        t.frame_start(8, 100 * MS);
        device_writes(&t, 0, [1; 16]);
        assert_eq!(t.check(105 * MS, Some(0)), None);
        assert_eq!(t.counts().stale, 1);
    }

    #[test]
    fn a_second_start_replaces_the_first() {
        let t = timer(2);
        t.frame_start(7, 100 * MS);
        t.frame_start(8, 108 * MS);
        assert_eq!(t.due(), Some(113 * MS));
        assert_eq!(t.counts().replaced, 1);
    }

    #[test]
    fn a_row_that_does_not_fit_its_buffer_is_refused() {
        assert!(mapped(2, 4096 - 16).is_some());
        assert!(mapped(2, 4096 - 15).is_none());
    }
}
