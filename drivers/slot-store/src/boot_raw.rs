//! The boot partition's records, on the partition's block node: the pending trial and the trial's
//! reason, each a two-copy record ([`crate::twocopy`]) at the sector [`crate::bootfat`] finds for its
//! pre-created file. Nothing mounts the partition and no file is written: a run-time update changes
//! two data sectors of one record and nothing else (see `docs/slots.md`, "Two-copy records").
//!
//! The trial's payload is `"<slot> <attempts>"`, the slot id and the attempts left, both as text. An
//! empty payload is no trial, which is what a cleared record holds. The reason's payload is the
//! reason's text, empty for none.

use crate::bootfat::{self, ReadAt, Refusal};
use crate::records::MAX_ATTEMPTS;
use crate::twocopy::{Error, Kind, Pair, Sectors};
use std::io;

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
        let trial = bootfat::locate(dev, TRIAL_FILE, RECORD_BYTES)?;
        let reason = bootfat::locate(dev, REASON_FILE, RECORD_BYTES)?;
        let episodes = bootfat::locate(dev, EPISODES_FILE, RECORD_BYTES)?;
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

#[cfg(test)]
#[path = "boot_raw_tests.rs"]
mod tests;
