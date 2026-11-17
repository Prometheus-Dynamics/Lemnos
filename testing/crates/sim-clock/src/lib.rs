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
/// How long a reader waits for a write in progress to finish, in wall time. A writer holds the
/// sequence odd only for two 8-byte writes, so a sequence still odd after this is a writer that is
/// gone or stopped, not one that was descheduled. This is the failure rule's cap (8 s), so the wait
/// is never a fixed number of tries.
const PATIENCE: Duration = Duration::from_secs(8);
/// Reads before the reader starts to sleep between tries. A sleeping reader gives the CPU to the
/// writer it is waiting for: a spinning one, sharing a CPU with it, starves it.
const SPINS: u32 = 64;
/// The longest sleep between two tries.
const SLEEP_CAP: Duration = Duration::from_millis(1);

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
        let mut sleep = Duration::from_micros(10);
        let mut tries = 0u32;
        loop {
            if let Some(ns) = self.read_once()? {
                let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
                if ns < *last {
                    return Err(format!("sim clock went backwards: {} -> {ns}", *last));
                }
                *last = ns;
                return Ok(ns);
            }
            if started.elapsed() >= PATIENCE {
                return Err(format!(
                    "clock file never settled within {} s: the writer is stuck mid-write",
                    PATIENCE.as_secs()
                ));
            }
            tries += 1;
            if tries <= SPINS {
                std::thread::yield_now();
            } else {
                std::thread::sleep(sleep);
                sleep = (sleep * 2).min(SLEEP_CAP);
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

    /// Writes the sequence and time raw, as a writer does, so a test can stop between the two.
    fn raw_write(file: &File, seq: u64, ns: u64) {
        file.write_all_at(&seq.to_le_bytes(), 0).unwrap();
        file.write_all_at(&ns.to_le_bytes(), 8).unwrap();
    }

    #[test]
    fn a_reader_waits_out_a_writer_stalled_mid_write() {
        // The writer leaves the sequence odd for 3 s, longer than the reader used to wait (about
        // 2.1 s, by try count). The reader must still get the time once the writer finishes.
        let path = scratch("stalled");
        let w = ClockWriter::create(&path, 5).unwrap();
        raw_write(&w.file, 3, 5);
        let reader = FileClock::open(&path).unwrap();
        let file = w.file;
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(3));
            raw_write(&file, 4, 9_000);
        });
        assert_eq!(reader.now_ns().unwrap(), 9_000);
        writer.join().unwrap();
    }

    #[test]
    fn a_writer_that_never_finishes_is_refused_after_the_stated_wait() {
        let path = scratch("never");
        let w = ClockWriter::create(&path, 5).unwrap();
        raw_write(&w.file, 3, 5);
        let reader = FileClock::open(&path).unwrap();
        let started = Instant::now();
        let error = reader.now_ns().unwrap_err();
        assert!(error.contains("stuck mid-write"), "{error}");
        assert!(
            started.elapsed() >= PATIENCE,
            "gave up after {:?}",
            started.elapsed()
        );
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
