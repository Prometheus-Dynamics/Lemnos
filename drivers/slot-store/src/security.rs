//! The board security switch: one value per board, off by default.
//!
//! Everything in the tree that gates on "security" reads it through [`enforced_at`] (drivers call
//! it with [`VIEW`]). The view is the two-line file `/etc/board/security`: `on` or `off`, then
//! `generation N`. The directory `/etc/board/` is a symlink on the image to the tmpfs directory
//! [`RUN_DIR`]; the daemon re-renders the file by writing a new file next to it and renaming it
//! over, so the inode changes and a reader never sees a partial file.
//!
//! Absent view = off. Present but unreadable or not exactly the two lines = corrupt = on.
//!
//! The truth is a record ([`Store`]): two 512-byte copies, each with a CRC and a sequence number,
//! so a cut leaves the old value or the new one. [`PlainFile`] keeps it in a plain file (the
//! boards without slots). Slot boards keep it on the boot partition through `twocopy` (I5i1).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::FileExt;

use crate::twocopy::{self, Kind, Pair, SECTOR, Sector, Sectors};
use std::path::{Path, PathBuf};

/// The one place the view's path is named. The reader contract for everyone: watch
/// [`VIEW_DIR`], read [`FILE_NAME`].
pub const VIEW_DIR: &str = "/etc/board";
/// The view a driver reads unless its config says otherwise (tests point it at a temp file).
pub const VIEW: &str = "/etc/board/security";
/// Where the daemon renders the view: a tmpfs directory the image links [`VIEW_DIR`] to.
pub const RUN_DIR: &str = "/run/board";
/// The view's file name inside both directories.
pub const FILE_NAME: &str = "security";
/// The name of the file the render writes before the rename.
const TEMP_NAME: &str = "security.tmp";

/// The switch's value and its generation (the record's sequence number; 0 when unknown).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Switch {
    pub on: bool,
    pub generation: u64,
}

/// What the view file says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// No file: the switch is off.
    Absent,
    /// A well-formed file.
    Set(Switch),
    /// A file that exists but cannot be read, or is not exactly the two lines: the switch is on.
    Corrupt,
}

/// Parse the view's bytes: exactly `on\n` or `off\n`, then `generation <decimal>\n`.
#[must_use]
pub fn parse(bytes: &[u8]) -> View {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return View::Corrupt;
    };
    let mut lines = text.split('\n');
    let on = match lines.next() {
        Some("on") => true,
        Some("off") => false,
        _ => return View::Corrupt,
    };
    let generation = match lines
        .next()
        .and_then(|line| line.strip_prefix("generation "))
    {
        Some(digits)
            if !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit())
                && digits.len() <= 20 =>
        {
            match digits.parse::<u64>() {
                Ok(value) => value,
                Err(_) => return View::Corrupt,
            }
        }
        _ => return View::Corrupt,
    };
    // Exactly two lines: the text ends after the second newline.
    if lines.next() != Some("") || lines.next().is_some() {
        return View::Corrupt;
    }
    View::Set(Switch { on, generation })
}

/// The exact bytes of a view.
#[must_use]
pub fn format(switch: Switch) -> String {
    let word = if switch.on { "on" } else { "off" };
    format!("{word}\ngeneration {}\n", switch.generation)
}

/// Read the view at `path`.
#[must_use]
pub fn read_at(path: &Path) -> View {
    match fs::read(path) {
        Ok(bytes) => parse(&bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => View::Absent,
        Err(_) => View::Corrupt,
    }
}

/// Whether the switch is on, as the view at `path` says.
#[must_use]
pub fn enforced_at(path: &Path) -> bool {
    match read_at(path) {
        View::Absent => false,
        View::Set(switch) => switch.on,
        View::Corrupt => true,
    }
}

#[cfg(feature = "testing")]
thread_local! {
    static TEST_VIEW: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Test seam (feature `testing`): points this thread's readers at `path`, or back at the board's
/// view with `None`. A shipped binary has no such hook, and no board file can name one.
#[cfg(feature = "testing")]
pub fn set_view_for_thread(path: Option<PathBuf>) {
    TEST_VIEW.with(|view| *view.borrow_mut() = path);
}

/// The view this thread reads: [`VIEW`], unless a test has pointed it elsewhere.
#[must_use]
pub fn view_path() -> PathBuf {
    #[cfg(feature = "testing")]
    if let Some(path) = TEST_VIEW.with(|view| view.borrow().clone()) {
        return path;
    }
    PathBuf::from(VIEW)
}

/// Whether the switch is on. The one reader every gate uses: slot installs and first flash.
#[must_use]
pub fn enforced() -> bool {
    enforced_at(&view_path())
}

/// Render the view into `dir` (normally [`RUN_DIR`]): a new file next to it, renamed over
/// `security`. Readers see the old file or the new one, never a partial one.
///
/// # Errors
///
/// Any I/O error. The old view stays in place when the write or the rename fails.
pub fn render_at(dir: &Path, switch: Switch) -> io::Result<()> {
    let temp = dir.join(TEMP_NAME);
    let mut file = File::create(&temp)?;
    file.write_all(format(switch).as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, dir.join(FILE_NAME))?;
    crate::store::sync_dir(dir)
}

/// The record that holds the truth: a store with a read and a write that is cut-safe.
pub trait Store {
    /// The recorded switch. `None` when there is no record at all (absent, which reads as off).
    ///
    /// # Errors
    ///
    /// `InvalidData` when a record is there but neither copy is valid (corrupt, which reads as
    /// on); any other I/O error.
    fn read(&self) -> io::Result<Option<Switch>>;

    /// Record `on` as the next generation and return that generation. Durable when it returns. A
    /// cut leaves the old record or the new one.
    ///
    /// # Errors
    ///
    /// Any I/O error. A failed write leaves the old value in place.
    fn write(&self, on: bool) -> io::Result<u64>;
}

/// The record's kind on the two-copy format: the board security switch (`twocopy`'s kind 4).
const KIND: Kind = Kind::SECURITY;
/// The record is the file's first two sectors.
const PAIR: Pair = Pair::at(0);

/// The record in one plain file of two sectors, kept by `twocopy`: each sector a copy, the newer
/// valid copy is the value, and a write overwrites only the copy that does not hold it. The file is
/// created by the first write, from a temporary file renamed into place, so a cut before the rename
/// leaves no record (absent, off) and never a torn one. Used on boards without slots; on slot boards
/// the record is the boot partition's (`twocopy`'s `Pair` on that device).
#[derive(Debug, Clone)]
pub struct PlainFile {
    path: PathBuf,
}

impl PlainFile {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Write `on` with at most `limit` bytes of the write reaching the file. A limit below one sector
    /// is the test's power cut: the write stops there, as the device would.
    pub(crate) fn write_limited(&self, on: bool, limit: usize) -> io::Result<u64> {
        if self.present()? {
            let file = OpenOptions::new().read(true).write(true).open(&self.path)?;
            let mut device = Device {
                file,
                limit: limit.min(SECTOR),
            };
            PAIR.write(&mut device, KIND, payload(on)).map_err(to_io)
        } else {
            self.create(on, limit)
        }
    }

    /// The first record: both copies in a temporary file, synced, renamed into place.
    fn create(&self, on: bool, limit: usize) -> io::Result<u64> {
        let mut sector = [0u8; SECTOR];
        twocopy::encode(KIND, 1, payload(on), &mut sector).map_err(to_io)?;
        let mut bytes = [0u8; 2 * SECTOR];
        bytes[..SECTOR].copy_from_slice(&sector);
        let temp = self.temp_path();
        let file = File::create(&temp)?;
        let cut = limit.min(2 * SECTOR);
        file.write_all_at(&bytes[..cut], 0)?;
        file.sync_all()?;
        drop(file);
        if cut < 2 * SECTOR {
            return Err(io::Error::other(
                "power cut before the record was renamed into place",
            ));
        }
        fs::rename(&temp, &self.path)?;
        sync_parent(&self.path)?;
        Ok(1)
    }

    fn temp_path(&self) -> PathBuf {
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(".new");
        self.path.with_file_name(name)
    }

    /// Whether there is a record to read: anything at the path except an empty file (which is absent).
    /// A directory or other non-file at the path is present, and reads as corrupt.
    fn present(&self) -> io::Result<bool> {
        match fs::metadata(&self.path) {
            Ok(meta) => Ok(!(meta.is_file() && meta.len() == 0)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

impl Store for PlainFile {
    fn read(&self) -> io::Result<Option<Switch>> {
        if !self.present()? {
            return Ok(None);
        }
        let file = File::open(&self.path)?;
        let mut device = Device {
            file,
            limit: SECTOR,
        };
        let current = PAIR.read(&mut device, KIND).map_err(to_io)?;
        match current {
            None => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "security record is corrupt: neither copy is valid",
            )),
            Some(current) => match current.payload.as_slice() {
                b"on" => Ok(Some(Switch {
                    on: true,
                    generation: current.seq,
                })),
                b"off" => Ok(Some(Switch {
                    on: false,
                    generation: current.seq,
                })),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "security record holds no switch value",
                )),
            },
        }
    }

    fn write(&self, on: bool) -> io::Result<u64> {
        self.write_limited(on, 2 * SECTOR)
    }
}

/// The payload of the switch's record: its value as the word the view uses.
fn payload(on: bool) -> &'static [u8] {
    if on { b"on" } else { b"off" }
}

fn to_io(error: twocopy::Error) -> io::Error {
    match error {
        twocopy::Error::Io(error) => error,
        other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
    }
}

/// The record's file as a `twocopy` device. A write that the limit cuts short writes its first bytes,
/// syncs them and fails, as a power cut would.
struct Device {
    file: File,
    limit: usize,
}

impl Sectors for Device {
    fn read(&mut self, lba: u64, buf: &mut Sector) -> io::Result<()> {
        self.file.read_exact_at(buf, lba * SECTOR as u64)
    }

    fn write(&mut self, lba: u64, buf: &Sector) -> io::Result<()> {
        let at = lba * SECTOR as u64;
        if self.limit < SECTOR {
            self.file.write_all_at(&buf[..self.limit], at)?;
            self.file.sync_data()?;
            return Err(io::Error::other("power cut mid-sector"));
        }
        self.file.write_all_at(buf, at)?;
        self.file.sync_data()
    }
}

/// Makes a rename durable: the directory holding `path` is synced.
fn sync_parent(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => File::open(dir)?.sync_all(),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::TempDir;

    fn on(generation: u64) -> Switch {
        Switch {
            on: true,
            generation,
        }
    }

    #[test]
    fn the_view_is_exactly_two_lines() {
        assert_eq!(parse(b"on\ngeneration 3\n"), View::Set(on(3)));
        assert_eq!(
            parse(b"off\ngeneration 0\n"),
            View::Set(Switch {
                on: false,
                generation: 0
            })
        );
        for bad in [
            &b""[..],
            b"on",
            b"on\ngeneration 3",
            b"on\ngeneration 3\n\n",
            b"on\ngeneration 3\nextra\n",
            b"yes\ngeneration 3\n",
            b"on\ngeneration -1\n",
            b"on\ngeneration \n",
            b"on\ngeneration 18446744073709551616\n",
            b"on\r\ngeneration 3\n",
            b"\xff\n",
        ] {
            assert_eq!(
                parse(bad),
                View::Corrupt,
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn absent_is_off_and_corrupt_is_on() {
        let dir = TempDir::new("view");
        let path = dir.0.join("security");
        assert!(!enforced_at(&path), "absent reads as off");
        fs::write(&path, b"off\ngeneration 1\n").unwrap();
        assert!(!enforced_at(&path));
        fs::write(&path, b"off\ngeneration 1").unwrap();
        assert!(enforced_at(&path), "a short file is corrupt: on");
        fs::create_dir(dir.0.join("dir")).unwrap();
        assert!(enforced_at(&dir.0.join("dir")), "unreadable is corrupt: on");
    }

    #[test]
    fn render_replaces_the_inode_and_leaves_no_partial_file() {
        let dir = TempDir::new("render");
        render_at(&dir.0, on(1)).unwrap();
        let first = fs::metadata(dir.0.join(FILE_NAME)).unwrap();
        let first_ino = std::os::unix::fs::MetadataExt::ino(&first);
        render_at(
            &dir.0,
            Switch {
                on: false,
                generation: 2,
            },
        )
        .unwrap();
        let second = fs::metadata(dir.0.join(FILE_NAME)).unwrap();
        assert_ne!(std::os::unix::fs::MetadataExt::ino(&second), first_ino);
        assert_eq!(
            fs::read_to_string(dir.0.join(FILE_NAME)).unwrap(),
            "off\ngeneration 2\n"
        );
        assert!(!dir.0.join(TEMP_NAME).exists());
    }

    #[test]
    fn a_reader_never_sees_a_partial_view_while_rendering() {
        let dir = TempDir::new("reader");
        render_at(&dir.0, on(0)).unwrap();
        let path = dir.0.join(FILE_NAME);
        let reader = {
            let path = path.clone();
            std::thread::spawn(move || {
                let mut seen = 0;
                for _ in 0..4000 {
                    match read_at(&path) {
                        View::Set(_) => seen += 1,
                        other => panic!("a reader saw {other:?} during a render"),
                    }
                }
                seen
            })
        };
        for generation in 1..=300u64 {
            render_at(
                &dir.0,
                Switch {
                    on: generation % 2 == 0,
                    generation,
                },
            )
            .unwrap();
        }
        assert_eq!(reader.join().unwrap(), 4000);
    }

    #[test]
    fn a_record_writes_each_generation_and_reads_the_newest() {
        let dir = TempDir::new("record");
        let store = PlainFile::new(dir.0.join("security.rec"));
        assert_eq!(store.read().unwrap(), None, "no file is no record");
        assert_eq!(store.write(true).unwrap(), 1);
        assert_eq!(store.read().unwrap(), Some(on(1)));
        assert_eq!(store.write(false).unwrap(), 2);
        assert_eq!(
            store.read().unwrap(),
            Some(Switch {
                on: false,
                generation: 2
            })
        );
        assert_eq!(store.write(true).unwrap(), 3);
        assert_eq!(store.read().unwrap(), Some(on(3)));
    }

    /// A power cut at each byte of the new sector: the old value always reads back, and the
    /// new one only once the whole sector is written. Never neither.
    #[test]
    fn a_cut_at_every_byte_leaves_the_old_or_the_new_value() {
        for cut in 0..=SECTOR {
            let dir = TempDir::new("cut");
            let store = PlainFile::new(dir.0.join("security.rec"));
            store.write(true).unwrap(); // generation 1: on
            // generation 2 (off) is cut after `cut` bytes of its sector.
            let _ = store.write_limited(false, cut);
            let got = store
                .read()
                .unwrap()
                .expect("one valid copy always remains");
            if cut == SECTOR {
                assert_eq!(
                    got,
                    Switch {
                        on: false,
                        generation: 2
                    },
                    "cut {cut}"
                );
            } else {
                assert_eq!(got, on(1), "cut {cut}: the old value must win");
            }
        }
    }

    #[test]
    fn a_cut_on_the_first_write_reads_as_absent_not_as_a_value() {
        for cut in 0..SECTOR {
            let dir = TempDir::new("cut-first");
            let store = PlainFile::new(dir.0.join("security.rec"));
            let _ = store.write_limited(true, cut);
            assert_eq!(store.read().unwrap(), None, "cut {cut}");
        }
    }

    #[test]
    fn a_damaged_copy_is_ignored() {
        let dir = TempDir::new("damage");
        let path = dir.0.join("security.rec");
        let store = PlainFile::new(&path);
        store.write(true).unwrap();
        store.write(false).unwrap();
        // Break the newest copy's CRC: the older copy (on, generation 1) wins.
        let mut bytes = fs::read(&path).unwrap();
        let newest = if bytes[SECTOR + 8] == 2 { SECTOR } else { 0 };
        bytes[newest + 20] ^= 0x40;
        fs::write(&path, &bytes).unwrap();
        assert_eq!(store.read().unwrap(), Some(on(1)));
    }
}
