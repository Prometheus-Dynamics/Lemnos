//! The simulated chain's clock, as a file: the simulator writes it, the chain proof reads it, in
//! another process. Test only: a board never has one (the proof's default clock is
//! `CLOCK_MONOTONIC`).
//!
//! The file is 16 bytes: a sequence number and the time, both little-endian `u64`, written as a
//! seqlock. The writer makes the sequence odd, writes the time, and makes the sequence even. A
//! reader takes the sequence, the time and the sequence again, and retries unless the two
//! sequences are equal and even. So a reader never returns a time that was not written, even
//! though an 8-byte `pwrite` is not guaranteed atomic. The time never goes backwards: the writer
//! refuses it, and so does the reader.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The file's length in bytes.
pub const LEN: u64 = 16;
/// Reads that may see a write in progress before a reader gives up. A writer holds the sequence
/// odd only for two 8-byte writes, so a reader that keeps losing is a broken writer.
/// Looks that yield before the reader starts to sleep. A writer that is runnable but not running
/// finishes its two-word write in a few scheduling slices; yields alone, a million of them, can
/// pass in less than one slice under load, which is how a reader gave up on a writer that was
/// still working.
const YIELDS: u32 = 1_000;
/// The longest sleep between looks, and the longest the reader waits for a writer that is stuck
/// mid-write (the failure rule's cap): past it the clock is reported as stuck.
const MAX_WAIT: Duration = Duration::from_secs(8);

/// A clock a proof can read. `now_ns` is the time in nanoseconds.
pub trait Clock: Send + Sync {
    /// The time now. An error names what was wrong; nothing falls back to another clock.
    fn now_ns(&self) -> Result<u64, String>;
}

/// The writer's side of the file.
pub struct ClockWriter {
    file: File,
    /// The last time written, so a write that goes backwards is refused.
    last: u64,
    seq: u64,
}

impl ClockWriter {
    /// Creates (or truncates) the file at `path` and writes `start_ns` as its time.
    ///
    /// # Errors
    /// The file cannot be created or written.
    pub fn create(path: &Path, start_ns: u64) -> io::Result<Self> {
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        let mut w = Self {
            file,
            last: start_ns,
            seq: 0,
        };
        w.write_raw(start_ns)?;
        Ok(w)
    }

    /// Sets the time to `ns`, which must not be earlier than the last time written.
    ///
    /// # Errors
    /// `ns` is earlier than the last time written, or the file cannot be written.
    pub fn set(&mut self, ns: u64) -> io::Result<()> {
        if ns < self.last {
            return Err(io::Error::other(format!(
                "sim clock would go backwards: {} -> {ns}",
                self.last
            )));
        }
        if ns == self.last {
            return Ok(());
        }
        self.write_raw(ns)?;
        self.last = ns;
        Ok(())
    }

    fn write_raw(&mut self, ns: u64) -> io::Result<()> {
        self.seq += 1; // odd: a write is in progress
        self.file.write_all_at(&self.seq.to_le_bytes(), 0)?;
        self.file.write_all_at(&ns.to_le_bytes(), 8)?;
        self.seq += 1; // even: the write is complete
        self.file.write_all_at(&self.seq.to_le_bytes(), 0)
    }
}

/// The reader's side: a [`Clock`] over the file, refusing a time earlier than one it has given.
pub struct FileClock {
    file: File,
    last: Mutex<u64>,
}

impl FileClock {
    /// Opens the file at `path`.
    ///
    /// # Errors
    /// The file is missing, cannot be opened, or is shorter than [`LEN`].
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("clock file {}: {e}", path.display()))?;
        let len = file
            .metadata()
            .map_err(|e| format!("clock file {}: {e}", path.display()))?
            .len();
        if len < LEN {
            return Err(format!(
                "clock file {} is {len} bytes, short of {LEN}",
                path.display()
            ));
        }
        Ok(Self {
            file,
            last: Mutex::new(0),
        })
    }

    fn read_once(&self) -> Result<Option<u64>, String> {
        let mut a = [0u8; 8];
        let mut v = [0u8; 8];
        let mut b = [0u8; 8];
        let read = |buf: &mut [u8], at: u64| {
            self.file
                .read_exact_at(buf, at)
                .map_err(|e| format!("clock file read: {e}"))
        };
        read(&mut a, 0)?;
        read(&mut v, 8)?;
        read(&mut b, 0)?;
        let (sa, sb) = (u64::from_le_bytes(a), u64::from_le_bytes(b));
        if sa != sb || sa % 2 == 1 {
            return Ok(None);
        }
        Ok(Some(u64::from_le_bytes(v)))
    }
}

impl Clock for FileClock {
    fn now_ns(&self) -> Result<u64, String> {
        let started = Instant::now();
        let mut looks = 0u32;
        let mut pause = Duration::from_micros(1);
        loop {
            if let Some(ns) = self.read_once()? {
                let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
                if ns < *last {
                    return Err(format!("sim clock went backwards: {} -> {ns}", *last));
                }
                *last = ns;
                return Ok(ns);
            }
            if started.elapsed() >= MAX_WAIT {
                return Err("clock file never settled: the writer is stuck mid-write".into());
            }
            looks += 1;
            if looks <= YIELDS {
                std::thread::yield_now();
            } else {
                std::thread::sleep(pause);
                pause = (pause * 2).min(MAX_WAIT);
            }
        }
    }
}

/// Whether a frame timestamp is not a frame start taken at `now`: in the future, or older than
/// `max_age_ns`. The chain proof's check, and the simulator's tests of it.
#[must_use]
pub fn age_bad(stamp: u64, now: u64, max_age_ns: u64) -> bool {
    stamp > now || now - stamp > max_age_ns
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sim-clock-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("clock")
    }

    #[test]
    fn a_reader_never_sees_a_time_that_was_not_written() {
        // Every value written is k * 1_000_003 + k: a torn read mixes two of them and fails it.
        let path = scratch("hammer");
        let mut w = ClockWriter::create(&path, 0).unwrap();
        let reader = FileClock::open(&path).unwrap();
        let writer = std::thread::spawn(move || {
            for k in 1..=200_000u64 {
                w.set(k * 1_000_003 + k).unwrap();
            }
        });
        let mut seen = 0u64;
        loop {
            let done = writer.is_finished();
            let ns = reader.now_ns().unwrap();
            // Written values are k * 1_000_004 for k in 0..=200_000; anything else is torn.
            assert_eq!(ns % 1_000_004, 0, "torn read {ns}");
            assert!(ns / 1_000_004 <= 200_000, "torn read {ns}");
            seen += 1;
            if done {
                break;
            }
        }
        writer.join().unwrap();
        assert!(seen > 0);
        assert_eq!(reader.now_ns().unwrap(), 200_000 * 1_000_003 + 200_000);
    }

    #[test]
    fn a_reader_waits_out_a_writer_that_is_stuck_mid_write() {
        // The writer is descheduled between the two halves of its write for five seconds. The
        // reader must wait it out and return the time written, not report the clock stuck.
        let path = scratch("stuck");
        let mut w = ClockWriter::create(&path, 5).unwrap();
        let reader = FileClock::open(&path).unwrap();
        // The first half of a write is on the file (odd sequence) and the second is not yet.
        w.file.write_all_at(&(w.seq + 1).to_le_bytes(), 0).unwrap();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(5));
            w.set(9_000).unwrap();
        });
        // The reader goes first, while the write is still open.
        assert_eq!(reader.now_ns(), Ok(9_000));
        writer.join().unwrap();
    }

    #[test]
    fn the_writer_and_reader_refuse_time_going_backwards() {
        let path = scratch("back");
        let mut w = ClockWriter::create(&path, 100).unwrap();
        assert!(w.set(50).is_err());
        w.set(200).unwrap();
        let reader = FileClock::open(&path).unwrap();
        assert_eq!(reader.now_ns().unwrap(), 200);
        // A file rewritten behind the reader's back, earlier, is refused.
        std::fs::write(&path, [0u8; 16]).unwrap();
        assert!(reader.now_ns().is_err());
    }

    #[test]
    fn a_missing_or_short_file_is_refused_by_name() {
        let path = scratch("missing");
        let _ = std::fs::remove_file(&path);
        let e = FileClock::open(&path).err().unwrap();
        assert!(e.contains("clock file"), "{e}");
        std::fs::write(&path, [0u8; 8]).unwrap();
        let e = FileClock::open(&path).err().unwrap();
        assert!(e.contains("short"), "{e}");
    }

    #[test]
    fn a_timestamp_in_the_future_or_too_old_is_not_a_frame_start() {
        let now = 10_000_000_000u64;
        let max = 1_000_000_000u64;
        assert!(age_bad(now + 1, now, max));
        assert!(age_bad(now - max - 1, now, max));
        assert!(!age_bad(now - 1_000_000, now, max));
        assert!(!age_bad(now, now, max));
    }
}
