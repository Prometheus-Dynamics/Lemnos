//! The crash log (`docs/slots.md`, "The crash log"): the previous boot's pstore record, put in the
//! boot history's crash index before pstore is cleared.
//!
//! A board declares the log with `crash_log` (the pstore mount, `/sys/fs/pstore` on a systemd image).
//! Unset, nothing here runs and the boot history is what it was. The driver never mounts pstore: a
//! declared log whose mount is missing is reported in `info` and the boot goes on.
//!
//! Each boot, once, [`record`] does this in order: it reads the records, appends the boot's history
//! entry, writes the record's text (its tail, bounded, with the cut said) and the index row, both by
//! `replace` (fsynced), and only then removes the records. A failure before the removal leaves pstore
//! as it was. The next boot finds the same record, and its key in the index keeps a second copy from
//! being stored.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use lemnos_slot_store::boots;
use lemnos_slot_store::store::{Layout, replace};

/// The state directory's folder of record texts, one file per boot: `crash/<n>`.
pub const TEXTS: &str = "crash";
/// The state directory's index: one line per stored record.
pub const INDEX: &str = "crashes";
/// The tail kept when `crash_bytes` is not given, in bytes.
pub const DEFAULT_BYTES: usize = 8192;
/// The largest `crash_bytes` a board may give.
pub const MAX_BYTES: usize = 65536;

/// One pstore record, as the kernel lists it. Its key for the index is all three fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    pub size: u64,
    pub mtime: u64,
}

/// The pstore records a board's crash log reads and clears. The driver's one implementation reads a
/// directory; the tests give a fake.
pub trait Pstore: Send {
    /// The directory the records are in, for the words `info` shows.
    fn dir(&self) -> PathBuf;
    /// Whether the directory is a mount of its own (pstore is mounted there).
    fn mounted(&self) -> bool;
    /// Every record in the directory.
    fn records(&self) -> io::Result<Vec<Record>>;
    /// The last `bytes` bytes of `record` (all of it when it is shorter).
    fn tail(&self, record: &Record, bytes: usize) -> io::Result<Vec<u8>>;
    /// Removes `record`.
    fn remove(&self, record: &Record) -> io::Result<()>;
}

/// The pstore directory on the running kernel.
pub struct FsPstore {
    dir: PathBuf,
}

impl FsPstore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
}

impl Pstore for FsPstore {
    fn dir(&self) -> PathBuf {
        self.dir.clone()
    }

    fn mounted(&self) -> bool {
        // A mount has its own device number: a directory that shares its parent's is not one.
        let Some(parent) = self.dir.parent() else {
            return false;
        };
        match (fs::metadata(&self.dir), fs::metadata(parent)) {
            (Ok(dir), Ok(parent)) => dir.is_dir() && dir.dev() != parent.dev(),
            _ => false,
        }
    }

    fn records(&self) -> io::Result<Vec<Record>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            if !meta.is_file() {
                continue;
            }
            out.push(Record {
                name: entry.file_name().to_string_lossy().into_owned(),
                size: meta.len(),
                mtime: u64::try_from(meta.mtime()).unwrap_or(0),
            });
        }
        Ok(out)
    }

    fn tail(&self, record: &Record, bytes: usize) -> io::Result<Vec<u8>> {
        let mut file = fs::File::open(self.dir.join(&record.name))?;
        let len = file.metadata()?.len();
        let keep = u64::try_from(bytes).unwrap_or(u64::MAX).min(len);
        file.seek(SeekFrom::Start(len - keep))?;
        let mut buf = vec![0u8; usize::try_from(keep).unwrap_or(0)];
        file.read_exact(&mut buf)?;
        Ok(buf)
    }

    fn remove(&self, record: &Record) -> io::Result<()> {
        fs::remove_file(self.dir.join(&record.name))
    }
}

/// The boot whose entry the record goes in: this boot's own, as the history writes it.
pub struct Start<'a> {
    pub boot: &'a str,
    pub slot: &'a str,
    pub outcome: &'a str,
    pub reset: &'a str,
    pub requested: &'a str,
    pub keep: usize,
}

/// What [`record`] did: whether the boot's history entry was written (`appended`, as the caller
/// takes the boot's request away on that), and the words `info` shows for the crash log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub appended: bool,
    pub note: String,
}

/// Records this boot in the history and, when the board declares a crash log, stores the previous
/// boot's record and clears pstore (see the module's order). `pstore` is the declared log and the
/// tail size in bytes; `None` is no crash log: the history is written as it always was.
pub fn record(layout: &Layout, start: Start, pstore: Option<(&dyn Pstore, usize)>) -> Recorded {
    let Some((pstore, bytes)) = pstore else {
        return Recorded {
            appended: append(layout, &start).is_ok(),
            note: String::new(),
        };
    };
    let dir = pstore.dir().display().to_string();
    if !pstore.mounted() {
        return Recorded {
            appended: append(layout, &start).is_ok(),
            note: format!("crash log: {dir} is not mounted"),
        };
    }
    // The records are read before anything is written.
    let listed = pstore.records();
    let chosen = listed
        .as_ref()
        .ok()
        .and_then(|records| pick(records))
        .cloned();
    let tail = match &chosen {
        Some(record) if !stored(layout, record) => Some(pstore.tail(record, bytes)),
        _ => None,
    };
    let appended = append(layout, &start);
    let records = match listed {
        Ok(records) => records,
        Err(err) => {
            return Recorded {
                appended: appended.is_ok(),
                note: format!("crash log: cannot read {dir}: {err}"),
            };
        }
    };
    let note = match (chosen, tail) {
        (None, _) if records.is_empty() => "crash log: no record from the last boot".to_string(),
        (None, _) => clear_note(
            pstore,
            &records,
            "crash log: no dmesg or console record from the last boot; pstore cleared",
        ),
        (Some(record), None) => clear_note(
            pstore,
            &records,
            &format!(
                "crash log: {} was stored by an earlier run; pstore cleared",
                record.name
            ),
        ),
        (Some(record), Some(Err(err))) => format!("crash log: cannot read {}: {err}", record.name),
        (Some(record), Some(Ok(tail))) => match entry_for(layout, start.boot) {
            None => format!(
                "crash log: {} kept on pstore: this boot has no history entry",
                record.name
            ),
            Some(n) => match store(layout, n, start.boot, &record, &tail) {
                Err(err) => format!("crash log: {} kept on pstore: {err}", record.name),
                Ok(cut) => {
                    let kept = tail.len();
                    let stored_note = format!(
                        "crash log: stored {} ({} bytes, kept {kept}, cut {cut}) as boot {n}",
                        record.name, record.size
                    );
                    match clear(pstore, &records) {
                        Ok(()) => format!("{stored_note}; pstore cleared"),
                        Err(err) => format!("{stored_note}, but pstore was not cleared: {err}"),
                    }
                }
            },
        },
    };
    Recorded {
        appended: appended.is_ok(),
        note,
    }
}

/// The record `info` and the index name first: the newest `dmesg-*`, else the newest `console-*`.
fn pick(records: &[Record]) -> Option<&Record> {
    let newest = |prefix: &str| {
        records
            .iter()
            .filter(|record| record.name.starts_with(prefix))
            .max_by(|a, b| (a.mtime, &a.name).cmp(&(b.mtime, &b.name)))
    };
    newest("dmesg-").or_else(|| newest("console-"))
}

fn clear(pstore: &dyn Pstore, records: &[Record]) -> io::Result<()> {
    let mut first = None;
    for record in records {
        if let Err(err) = pstore.remove(record) {
            first.get_or_insert(err);
        }
    }
    first.map_or(Ok(()), Err)
}

fn clear_note(pstore: &dyn Pstore, records: &[Record], done: &str) -> String {
    match clear(pstore, records) {
        Ok(()) => done.to_string(),
        Err(err) => format!("{done}; clearing pstore failed: {err}"),
    }
}

fn append(layout: &Layout, start: &Start) -> io::Result<bool> {
    boots::append(
        layout,
        start.boot,
        start.slot,
        start.outcome,
        start.reset,
        start.requested,
        start.keep,
    )
}

/// This boot's history entry number, when it has one.
fn entry_for(layout: &Layout, boot: &str) -> Option<u64> {
    let boot = boots::field(boot);
    boots::load(layout)
        .iter()
        .rev()
        .find(|entry| entry.boot == boot)
        .map(|entry| entry.n)
}

/// One index row: the boot it was stored for, the record's key, what was kept, and its status.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    n: u64,
    boot: String,
    record: Record,
    kept: u64,
}

impl Row {
    fn line(&self) -> String {
        format!(
            "{} {} {} {} {} {} stored\n",
            self.n,
            boots::field(&self.boot),
            boots::field(&self.record.name),
            self.record.size,
            self.record.mtime,
            self.kept
        )
    }

    fn parse(line: &str) -> Option<Self> {
        let mut parts = line.split_whitespace();
        let row = Self {
            n: parts.next()?.parse().ok()?,
            boot: parts.next()?.to_string(),
            record: Record {
                name: parts.next()?.to_string(),
                size: parts.next()?.parse().ok()?,
                mtime: parts.next()?.parse().ok()?,
            },
            kept: parts.next()?.parse().ok()?,
        };
        (parts.next()? == "stored" && parts.next().is_none()).then_some(row)
    }
}

fn index(layout: &Layout) -> Vec<Row> {
    fs::read_to_string(layout.marker(INDEX))
        .map(|text| text.lines().filter_map(Row::parse).collect())
        .unwrap_or_default()
}

/// Whether the index already holds this record (its name, size and mtime).
fn stored(layout: &Layout, record: &Record) -> bool {
    index(layout).iter().any(|row| &row.record == record)
}

/// Writes the record's text and its index row for boot `n`, both fsynced, then drops the rows and
/// texts of boots the history no longer keeps. Returns the bytes cut before the tail.
fn store(layout: &Layout, n: u64, boot: &str, record: &Record, tail: &[u8]) -> io::Result<u64> {
    let cut = record.size.saturating_sub(tail.len() as u64);
    let mut text = Vec::with_capacity(tail.len() + 64);
    if cut > 0 {
        text.extend_from_slice(format!("cut: {cut} bytes before this not kept\n").as_bytes());
    }
    text.extend_from_slice(tail);
    let texts = layout.state_dir().join(TEXTS);
    fs::create_dir_all(&texts)?;
    replace(&texts.join(n.to_string()), &text)?;

    let mut rows = index(layout);
    rows.push(Row {
        n,
        boot: boot.to_string(),
        record: record.clone(),
        kept: tail.len() as u64,
    });
    let kept_boots: Vec<u64> = boots::load(layout).iter().map(|entry| entry.n).collect();
    rows.retain(|row| kept_boots.contains(&row.n));
    let mut body = String::new();
    for row in &rows {
        body.push_str(&row.line());
    }
    replace(&layout.marker(INDEX), body.as_bytes())?;
    // The texts of the boots the index no longer holds go too; a failure to remove one is harmless.
    if let Ok(entries) = fs::read_dir(&texts) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let held = name
                .parse::<u64>()
                .is_ok_and(|held| rows.iter().any(|row| row.n == held));
            if !held {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    Ok(cut)
}
