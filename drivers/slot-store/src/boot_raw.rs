//! The boot partition's records, on the partition's block node: the pending trial and the trial's
//! reason, each a two-copy record ([`crate::twocopy`]) at the sector [`crate::bootfat`] finds for its
//! pre-created file. Nothing mounts the partition and no file is written: a run-time update changes
//! two data sectors of one record and nothing else (see `docs/slots.md`, "Two-copy records").
//!
//! The trial's payload is `"<slot> <attempts>"`, the slot id and the attempts left, both as text. An
//! empty payload is no trial, which is what a cleared record holds. The reason's payload is the
//! reason's text, empty for none.

use crate::bootfat::{self, ReadAt, Refusal};
use crate::firmware;
use crate::records::MAX_ATTEMPTS;
use crate::twocopy::{Error, FileSectors, Kind, Pair, SECTOR, Sectors};
use std::fs::OpenOptions;
use std::io;
use std::path::Path;

/// The FAT directory name of the trial record's file.
pub const TRIAL_FILE: &[u8; 11] = b"TRIAL   REC";
/// The FAT directory name of the reason record's file.
pub const REASON_FILE: &[u8; 11] = b"REASON  REC";
/// The FAT directory name of the episodes record's file.
pub const EPISODES_FILE: &[u8; 11] = b"EPISODESREC";
/// The size of each record file: two sectors, one per copy.
pub const RECORD_BYTES: u64 = 1024;

/// Where the two records are on the partition, found once from the boot sector and the directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Records {
    trial: Pair,
    reason: Pair,
    episodes: Pair,
}

/// A pending trial: the slot and the attempts left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trial {
    /// The trial's slot id.
    pub slot: String,
    /// Boots that may still start the trial (at most [`MAX_ATTEMPTS`]).
    pub attempts: u8,
}

impl Records {
    /// Finds both record files, read-only. Each refusal names the file, and nothing is written.
    pub fn place<D: ReadAt>(dev: &mut D) -> Result<Self, Refusal> {
        Self::place_named(dev).map_err(|(_, why)| why)
    }

    /// As [`Records::place`], refusing by the record file's name (`TRIAL   REC` and so on).
    pub fn place_named<D: ReadAt>(dev: &mut D) -> Result<Self, (&'static str, Refusal)> {
        let mut named = |name: &'static [u8; 11]| {
            bootfat::locate(dev, name, RECORD_BYTES)
                .map_err(|why| (std::str::from_utf8(name).unwrap_or("?"), why))
        };
        let trial = named(TRIAL_FILE)?;
        let reason = named(REASON_FILE)?;
        let episodes = named(EPISODES_FILE)?;
        Ok(Self {
            trial: Pair::at(trial.first_lba),
            reason: Pair::at(reason.first_lba),
            episodes: Pair::at(episodes.first_lba),
        })
    }

    /// The first sector of each record's copy A: trial, then reason, for tests and the image
    /// builder's report. The episodes record's is [`Records::episodes_lba`].
    pub fn lbas(&self) -> (u64, u64) {
        (self.trial.lba(), self.reason.lba())
    }

    /// The first sector of the episodes record's copy A.
    pub fn episodes_lba(&self) -> u64 {
        self.episodes.lba()
    }

    /// The episodes recorded against a slot: `(slot, episodes)`, `None` when none is kept.
    pub fn episodes<D: Sectors>(&self, dev: &mut D) -> Result<Option<(String, u8)>, Error> {
        let Some(cur) = self.episodes.read(dev, Kind::EPISODES)? else {
            return Ok(None);
        };
        Ok(decode_episodes(&cur.payload))
    }

    /// Records `episodes` against `slot`, in one sector write. Zero is written as an empty payload.
    pub fn set_episodes<D: Sectors>(
        &self,
        dev: &mut D,
        slot: &str,
        episodes: u8,
    ) -> Result<(), Error> {
        if !crate::store::valid_id(slot) {
            return Err(invalid("episodes slot id is not valid"));
        }
        let payload = if episodes == 0 {
            String::new()
        } else {
            format!("{slot} {episodes}")
        };
        self.episodes
            .write(dev, Kind::EPISODES, payload.as_bytes())?;
        Ok(())
    }

    /// Clears the episodes record (a boot-good on the slot).
    pub fn clear_episodes<D: Sectors>(&self, dev: &mut D) -> Result<(), Error> {
        self.episodes.write(dev, Kind::EPISODES, b"")?;
        Ok(())
    }

    /// The pending trial, `None` when the record is absent, empty or holds no trial.
    pub fn trial<D: Sectors>(&self, dev: &mut D) -> Result<Option<Trial>, Error> {
        let Some(cur) = self.trial.read(dev, Kind::TRIAL)? else {
            return Ok(None);
        };
        Ok(decode_trial(&cur.payload))
    }

    /// Makes `slot` the pending trial with `attempts` left, in one sector write.
    pub fn set_trial<D: Sectors>(
        &self,
        dev: &mut D,
        slot: &str,
        attempts: u8,
    ) -> Result<(), Error> {
        if !crate::store::valid_id(slot) {
            return Err(invalid("trial slot id is not valid"));
        }
        if attempts > MAX_ATTEMPTS {
            return Err(invalid("trial attempts are more than the loader's maximum"));
        }
        let text = format!("{slot} {attempts}");
        self.trial.write(dev, Kind::TRIAL, text.as_bytes())?;
        Ok(())
    }

    /// Takes the pending trial out, in one sector write.
    pub fn clear_trial<D: Sectors>(&self, dev: &mut D) -> Result<(), Error> {
        self.trial.write(dev, Kind::TRIAL, b"")?;
        Ok(())
    }

    /// The reason the last trial fell back, if one is kept.
    pub fn reason<D: Sectors>(&self, dev: &mut D) -> Result<Option<String>, Error> {
        let Some(cur) = self.reason.read(dev, Kind::REASON)? else {
            return Ok(None);
        };
        if cur.payload.is_empty() {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&cur.payload).into_owned()))
    }

    /// Keeps `reason` (newlines replaced by spaces), in one sector write.
    pub fn set_reason<D: Sectors>(&self, dev: &mut D, reason: &str) -> Result<(), Error> {
        let text = reason.replace(['\n', '\r'], " ");
        self.reason.write(dev, Kind::REASON, text.as_bytes())?;
        Ok(())
    }
}

/// A trial's payload as [`Records::set_trial`] writes it: `None` for an empty or malformed one.
pub fn decode_trial(payload: &[u8]) -> Option<Trial> {
    let text = std::str::from_utf8(payload).ok()?;
    if text.is_empty() {
        return None;
    }
    let (slot, attempts) = text.split_once(' ')?;
    let attempts = crate::records::attempts(attempts)?;
    crate::store::valid_id(slot).then(|| Trial {
        slot: slot.to_string(),
        attempts,
    })
}

/// An episodes payload: `None` for an empty or malformed one.
pub fn decode_episodes(payload: &[u8]) -> Option<(String, u8)> {
    let text = std::str::from_utf8(payload).ok()?;
    let (slot, n) = text.split_once(' ')?;
    let n: u8 = n.parse().ok()?;
    (n > 0 && crate::store::valid_id(slot)).then(|| (slot.to_string(), n))
}

fn invalid(why: &'static str) -> Error {
    Error::Io(io::Error::new(io::ErrorKind::InvalidInput, why))
}

/// The FAT directory names of the three pointer files: one sector each, read by the Pi firmware as
/// `default.txt`, `previous.txt` and `trial.txt` (see [`crate::firmware`]).
pub const DEFAULT_POINTER: &[u8; 11] = b"DEFAULT TXT";
/// See [`DEFAULT_POINTER`].
pub const PREVIOUS_POINTER: &[u8; 11] = b"PREVIOUSTXT";
/// See [`DEFAULT_POINTER`].
pub const TRIAL_POINTER: &[u8; 11] = b"TRIAL   TXT";

/// One of the pointer files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pointer {
    /// `default.txt`: the slot the firmware starts.
    Default,
    /// `previous.txt`: the slot a count-based fallback starts.
    Previous,
    /// `trial.txt`: the trial slot, which `tryboot.txt` includes.
    Trial,
}

impl Pointer {
    /// The FAT directory name of the file.
    pub const fn file_name(self) -> &'static [u8; 11] {
        match self {
            Self::Default => DEFAULT_POINTER,
            Self::Previous => PREVIOUS_POINTER,
            Self::Trial => TRIAL_POINTER,
        }
    }

    /// The name the firmware reads the file by, for messages.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Default => "default.txt",
            Self::Previous => "previous.txt",
            Self::Trial => "trial.txt",
        }
    }
}

/// Where the three pointer files are on the partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pointers {
    lbas: [u64; 3],
}

impl Pointers {
    /// Finds the three pointer files, read-only. A missing, fragmented or wrongly sized file is
    /// refused by name, and nothing is written.
    pub fn place<D: ReadAt>(dev: &mut D) -> Result<Self, (&'static str, Refusal)> {
        let mut lbas = [0; 3];
        for which in [Pointer::Default, Pointer::Previous, Pointer::Trial] {
            let found = bootfat::locate(dev, which.file_name(), SECTOR as u64)
                .map_err(|why| (which.name(), why))?;
            lbas[which as usize] = found.first_lba;
        }
        Ok(Self { lbas })
    }

    /// The sector of `which`.
    pub fn lba(&self, which: Pointer) -> u64 {
        self.lbas[which as usize]
    }

    /// The slot the file names, `None` when the sector is torn or is not a pointer.
    pub fn read<D: Sectors>(&self, dev: &mut D, which: Pointer) -> io::Result<Option<String>> {
        let mut sector = [0u8; SECTOR];
        dev.read(self.lba(which), &mut sector)?;
        Ok(firmware::pointer_slot(&sector))
    }

    /// Points `which` at slot `id`: one sector write, durable when it returns.
    pub fn write<D: Sectors>(&self, dev: &mut D, which: Pointer, id: &str) -> Result<(), Error> {
        let sector =
            firmware::pointer_sector(id).ok_or_else(|| invalid("pointer slot id is not valid"))?;
        dev.write(self.lba(which), &sector).map_err(Error::Io)
    }
}

/// The boot partition's raw records, as the code that changes them sees them. Each method is one
/// step that is durable when it returns, so a cut between two steps leaves each file old or new.
/// The daemon and slot-init use [`Boot`]; their tests use an in-memory fake.
pub trait BootRaw {
    /// The slot pointer `which` names, `None` when it is torn or not a pointer.
    fn pointer(&mut self, which: Pointer) -> Result<Option<String>, String>;
    /// Points `which` at `slot`.
    fn set_pointer(&mut self, which: Pointer, slot: &str) -> Result<(), String>;
    /// The pending trial, if any.
    fn trial(&mut self) -> Result<Option<Trial>, String>;
    /// Makes `slot` the pending trial with `attempts` left (the record only).
    fn set_trial(&mut self, slot: &str, attempts: u8) -> Result<(), String>;
    /// Takes the pending trial out (the record only).
    fn clear_trial(&mut self) -> Result<(), String>;
    /// The kept reason of the last held trial, `None` when none is kept.
    fn reason(&mut self) -> Result<Option<String>, String>;
    /// Keeps `reason` in the reason record (the record only; empty text clears it).
    fn set_reason(&mut self, reason: &str) -> Result<(), String>;
    /// The episodes recorded against a slot, `None` when none are.
    fn episodes(&mut self) -> Result<Option<(String, u8)>, String>;
    /// Records `episodes` against `slot` (zero clears the record).
    fn set_episodes(&mut self, slot: &str, episodes: u8) -> Result<(), String>;
    /// Clears the episodes record.
    fn clear_episodes(&mut self) -> Result<(), String>;

    /// Arms a trial: the trial pointer first, then the record. A cut after the pointer leaves no
    /// record, so no tryboot restart happens and the old pointer is never read; the record is written
    /// only once the pointer names its slot.
    fn arm_trial(&mut self, slot: &str, attempts: u8) -> Result<(), String> {
        self.set_pointer(Pointer::Trial, slot)?;
        self.set_trial(slot, attempts)
    }
}

/// Arms a trial on the partition: the trial pointer first, then the record. A cut after the pointer
/// leaves no record, so no tryboot restart happens and the old pointer is never read; the record is
/// written only once the pointer names its slot.
pub fn arm_trial_raw<D: Sectors>(
    records: &Records,
    pointers: &Pointers,
    dev: &mut D,
    slot: &str,
    attempts: u8,
) -> Result<(), Error> {
    pointers.write(dev, Pointer::Trial, slot)?;
    records.set_trial(dev, slot, attempts)
}

/// The boot partition's block node, opened for the raw records and pointers. Nothing mounts it.
pub struct Boot {
    dev: FileSectors,
    records: Records,
    pointers: Pointers,
}

impl Boot {
    /// Opens the node read-write and finds every record and pointer file on it. A file that is
    /// missing or not placeable is refused by name.
    pub fn open(node: &Path) -> Result<Self, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(node)
            .map_err(|e| format!("{}: {e}", node.display()))?;
        let mut dev = FileSectors::new(file, 0);
        let records = Records::place_named(&mut dev)
            .map_err(|(name, why)| format!("{}: {name} {why}", node.display()))?;
        let pointers = Pointers::place(&mut dev)
            .map_err(|(name, why)| format!("{}: {name} {why}", node.display()))?;
        Ok(Self {
            dev,
            records,
            pointers,
        })
    }
}

impl BootRaw for Boot {
    fn pointer(&mut self, which: Pointer) -> Result<Option<String>, String> {
        self.pointers
            .read(&mut self.dev, which)
            .map_err(|e| e.to_string())
    }

    fn set_pointer(&mut self, which: Pointer, slot: &str) -> Result<(), String> {
        self.pointers
            .write(&mut self.dev, which, slot)
            .map_err(|e| e.to_string())
    }

    fn trial(&mut self) -> Result<Option<Trial>, String> {
        self.records.trial(&mut self.dev).map_err(|e| e.to_string())
    }

    fn set_trial(&mut self, slot: &str, attempts: u8) -> Result<(), String> {
        self.records
            .set_trial(&mut self.dev, slot, attempts)
            .map_err(|e| e.to_string())
    }

    fn arm_trial(&mut self, slot: &str, attempts: u8) -> Result<(), String> {
        arm_trial_raw(&self.records, &self.pointers, &mut self.dev, slot, attempts)
            .map_err(|e| e.to_string())
    }

    fn clear_trial(&mut self) -> Result<(), String> {
        self.records
            .clear_trial(&mut self.dev)
            .map_err(|e| e.to_string())
    }

    fn reason(&mut self) -> Result<Option<String>, String> {
        self.records
            .reason(&mut self.dev)
            .map_err(|e| e.to_string())
    }

    fn set_reason(&mut self, reason: &str) -> Result<(), String> {
        self.records
            .set_reason(&mut self.dev, reason)
            .map_err(|e| e.to_string())
    }

    fn episodes(&mut self) -> Result<Option<(String, u8)>, String> {
        self.records
            .episodes(&mut self.dev)
            .map_err(|e| e.to_string())
    }

    fn set_episodes(&mut self, slot: &str, episodes: u8) -> Result<(), String> {
        self.records
            .set_episodes(&mut self.dev, slot, episodes)
            .map_err(|e| e.to_string())
    }

    fn clear_episodes(&mut self) -> Result<(), String> {
        self.records
            .clear_episodes(&mut self.dev)
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
#[path = "boot_raw_tests.rs"]
mod tests;
