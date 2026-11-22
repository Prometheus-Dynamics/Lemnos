//! The boot partition's records, on the partition's block node: the pending trial and the trial's
//! reason, each a two-copy record ([`crate::twocopy`]) at the sector [`crate::bootfat`] finds for its
//! pre-created file. Nothing mounts the partition and no file is written: a run-time update changes
//! two data sectors of one record and nothing else (see `docs/slots.md`, "Two-copy records").
//!
//! The trial's payload is `"<slot> <attempts>"`, the slot id and the attempts left, both as text. An
//! empty payload is no trial, which is what a cleared record holds. The reason's payload is the
//! reason's text, empty for none.

use crate::bays;
use crate::bootfat::{self, ReadAt, Refusal};
use crate::firmware;
use crate::records::MAX_ATTEMPTS;
use crate::twocopy::{Error, FileSectors, Kind, Pair, SECTOR, Sectors};
use std::io;
use std::path::Path;

/// The FAT directory name of the trial record's file.
pub const TRIAL_FILE: &[u8; 11] = b"TRIAL   REC";
/// The FAT directory name of the reason record's file.
pub const REASON_FILE: &[u8; 11] = b"REASON  REC";
/// The FAT directory name of the episodes record's file.
pub const EPISODES_FILE: &[u8; 11] = b"EPISODESREC";

/// The FAT directory name of the restart baseline record (`count.rec`, M36b). Optional: an image
/// without it boots, and its flash decision refuses by name (see [`crate::firmware::since_good`]).
pub const COUNT_FILE: &[u8; 11] = b"COUNT   REC";

/// The baseline record's payload: the count as decimal text. Zero is `0`, so empty is only "unknown".
pub fn encode_baseline(count: u32) -> String {
    count.to_string()
}

/// A baseline payload: `None` for an empty (unknown) or malformed one.
pub fn decode_baseline(payload: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(payload).ok()?;
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}
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

/// The FAT directory names of the three pointer files: two sectors each (one form, I5i8), read by the Pi firmware as
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
            let found =
                bootfat::locate(dev, which.file_name(), firmware::POINTER_FILE_BYTES as u64)
                    .map_err(|why| (which.name(), why))?;
            lbas[which as usize] = found.first_lba;
        }
        Ok(Self { lbas })
    }

    /// The sector of `which`.
    pub fn lba(&self, which: Pointer) -> u64 {
        self.lbas[which as usize]
    }

    /// The slot the file names: the last whole line of its two sectors, `None` when neither is one.
    pub fn read<D: Sectors>(&self, dev: &mut D, which: Pointer) -> io::Result<Option<String>> {
        let mut file = [0u8; firmware::POINTER_FILE_BYTES];
        let lba = self.lba(which);
        for (k, chunk) in file.chunks_mut(SECTOR).enumerate() {
            dev.read(lba + k as u64, chunk.try_into().expect("a whole sector"))?;
        }
        Ok(firmware::pointer_file_slot(&file))
    }

    /// Points `which` at slot `id`: the second sector, then the first; each one a durable write. A cut
    /// after the second leaves the new line in the file, so the commit is that write.
    pub fn write<D: Sectors>(&self, dev: &mut D, which: Pointer, id: &str) -> Result<(), Error> {
        let file =
            firmware::pointer_file(id).ok_or_else(|| invalid("pointer slot id is not valid"))?;
        let lba = self.lba(which);
        for k in [1usize, 0] {
            let at = k * SECTOR;
            dev.write(
                lba + k as u64,
                file[at..at + SECTOR].try_into().expect("a whole sector"),
            )
            .map_err(Error::Io)?;
        }
        Ok(())
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
    /// The bay a slot's files are in: the set's directory (`slots/bayN`) and each payload's stored
    /// length from the set's record. Refused by name when no set holds the slot.
    fn slot_bay(&mut self, slot: &str) -> Result<(String, Vec<(String, u64)>), String>;
    /// The restart count the last good boot (or the first boot) started from: `None` when unknown,
    /// or when the image has no baseline record. The default refuses by name.
    fn baseline(&mut self) -> Result<Option<u32>, String> {
        Err("this boot partition has no restart baseline record".into())
    }
    /// Keeps `count` as the restart baseline (the record only).
    fn set_baseline(&mut self, _count: u32) -> Result<(), String> {
        Err("this boot partition has no restart baseline record".into())
    }

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

/// Whether a boot partition's records and pointers are this system's, as the record writer decides
/// it: every record and pointer file is found at the size the image builder writes, and every bay
/// set's record is this layout version's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ownership {
    /// Every record and pointer file is there at its size.
    Ours,
    /// A record or pointer file is missing or another size: another system's boot partition (a
    /// slot added with `--add-to`, whose `default.txt` is the existing system's). The text names it.
    NotOurs(String),
    /// The partition could not be read far enough to say (a device or FAT fault). The text names it.
    Unknown(String),
}

/// Reads `dev` only and says whether its records and pointers are ours: the same placement
/// [`Boot::on`] makes before any write, so a partition the writer would refuse as "not ours" is
/// [`Ownership::NotOurs`] here, and one it would open is [`Ownership::Ours`].
pub fn ownership<D: ReadAt + Sectors>(dev: &mut D) -> Ownership {
    let placed = Records::place_named(dev)
        .map(drop)
        .and_then(|()| Pointers::place(dev).map(drop));
    match placed {
        Ok(()) => match bays::records_version(dev) {
            Ok(None) => Ownership::Ours,
            Ok(Some(why)) => Ownership::NotOurs(why),
            Err(why) => Ownership::Unknown(why),
        },
        Err((name, why @ (Refusal::Missing | Refusal::WrongSize { .. }))) => {
            Ownership::NotOurs(format!("{name} {why}"))
        }
        Err((name, why)) => Ownership::Unknown(format!("{name} {why}")),
    }
}

/// [`ownership`] of the boot partition at `node`, opened read only.
pub fn ownership_of(node: &Path) -> Ownership {
    match crate::boot_node::open(node, false) {
        Ok(file) => ownership(&mut FileSectors::new(file, 0)),
        Err(e) => Ownership::Unknown(format!("{}: {e}", node.display())),
    }
}

/// What the daemon's root phase did with a boot node (see [`prepare`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prepared {
    /// The records are ours: the node is handed over read-write.
    ReadWrite,
    /// The records are not ours, or can't be read to say: the node is handed over read only, and
    /// nothing on it can be written. The text says why.
    ReadOnly(String),
    /// The node could not be opened at all; nothing is handed over. The text says why.
    Failed(String),
}

/// The root phase's step for a boot node, before the daemon drops to its own user: the node is
/// opened read only and the ownership probe runs on it; only when the records are ours is it opened
/// read-write, and that file is handed over ([`crate::boot_node::hand_over`]). On another system's
/// partition, or one that can't be read to say, the read-only file is handed over instead, so the
/// dropped daemon holds no way to write it.
pub fn prepare(node: &Path) -> Prepared {
    use std::fs::OpenOptions;
    let read = match OpenOptions::new().read(true).open(node) {
        Ok(file) => file,
        Err(e) => return Prepared::Failed(format!("{}: {e}", node.display())),
    };
    let probe = match read.try_clone() {
        Ok(file) => file,
        Err(e) => return Prepared::Failed(format!("{}: {e}", node.display())),
    };
    let why = match ownership(&mut FileSectors::new(probe, 0)) {
        Ownership::Ours => match OpenOptions::new().read(true).write(true).open(node) {
            Ok(file) => {
                crate::boot_node::hand_over(node, file, true);
                return Prepared::ReadWrite;
            }
            Err(e) => format!(
                "the records are ours, but {} can't be opened read-write: {e}",
                node.display()
            ),
        },
        Ownership::NotOurs(why) => format!("not ours: {why}"),
        Ownership::Unknown(why) => format!("can't be read to say whose: {why}"),
    };
    crate::boot_node::hand_over(node, read, false);
    Prepared::ReadOnly(why)
}

/// The boot partition's block node, opened for the raw records and pointers. Nothing mounts it.
pub struct Boot<D = FileSectors> {
    dev: D,
    records: Records,
    pointers: Pointers,
    /// The restart baseline record's first copy, when the image has the file (`count.rec`).
    baseline: Option<Pair>,
    /// The bay layout the image was built with; `Err` names why it could not be read.
    layout: Result<bays::Layout, String>,
}

impl Boot<FileSectors> {
    /// Opens the node read-write and finds every record and pointer file on it. A file that is
    /// missing or not placeable is refused by name.
    pub fn open(node: &Path) -> Result<Self, String> {
        let file =
            crate::boot_node::open(node, true).map_err(|e| format!("{}: {e}", node.display()))?;
        Boot::on(FileSectors::new(file, 0)).map_err(|e| format!("{}: {e}", node.display()))
    }
}

impl<D> Boot<D> {
    /// The device the boot partition is read and written through.
    pub fn into_device(self) -> D {
        self.dev
    }

    /// The bay sizes the image was built with, read when the image was opened; `Err` names why
    /// they could not be read.
    pub fn bay_layout(&self) -> Result<bays::Layout, String> {
        self.layout.clone()
    }
}

impl<D: Sectors + ReadAt> Boot<D> {
    /// The partition's `config.txt`, read only. Lemnos never writes it at run time.
    pub fn config_text(&mut self) -> Result<Vec<u8>, String> {
        bootfat::read_file(&mut self.dev, "config.txt", 65536)
            .map_err(|e| format!("config.txt: {e}"))
    }

    /// The boot partition on `dev` (a node, or an image in a test): every record and pointer file is
    /// found, and the bay layout read. A file that is missing or not placeable is refused by name.
    pub fn on(mut dev: D) -> Result<Self, String> {
        let records =
            Records::place_named(&mut dev).map_err(|(name, why)| format!("{name} {why}"))?;
        let pointers = Pointers::place(&mut dev).map_err(|(name, why)| format!("{name} {why}"))?;
        // A partition made for another layout version is not opened: nothing on it is written.
        if let Some(why) = bays::records_version(&mut dev)? {
            return Err(why);
        }
        // Optional: an older image without the file boots, and its flash decision refuses by name.
        let baseline = bootfat::locate(&mut dev, COUNT_FILE, RECORD_BYTES)
            .ok()
            .map(|found| Pair::at(found.first_lba));
        let layout = bays::discover(&mut dev);
        Ok(Self {
            dev,
            records,
            pointers,
            baseline,
            layout,
        })
    }

    /// The bay set a slot's committed record is in, refused by name when none is.
    fn set_for(&mut self, slot: &str) -> Result<u8, String> {
        self.layout.clone()?;
        bays::set_of_slot(&mut self.dev, slot)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                format!("slot {slot} has no bay on the boot partition: nothing names it")
            })
    }

    /// The slot a pointer's set holds, from that set's record.
    fn slot_of_pointer(&mut self, which: Pointer) -> Result<Option<String>, String> {
        let Some(name) = self
            .pointers
            .read(&mut self.dev, which)
            .map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        self.layout.clone()?;
        let Some(set) = bays::set_of(&name) else {
            return Ok(None);
        };
        let placed = bays::place(&mut self.dev)?;
        let info =
            bays::read_info(&mut self.dev, placed.info[set as usize]).map_err(|e| e.to_string())?;
        Ok(info.and_then(|i| i.slot))
    }

    /// The bay set a slot's files are in: what the boot chain reads. Refused by name when none is.
    pub fn bay_of(&mut self, slot: &str) -> Result<u8, String> {
        self.set_for(slot)
    }

    /// The bay set a slot is in, and the record that set holds: each boot file's stored length and
    /// hash. Refused by name when the slot has no bay or the bay holds no record.
    pub fn bay_record(&mut self, slot: &str) -> Result<(u8, bays::Info), String> {
        let set = self.set_for(slot)?;
        let placed = bays::place(&mut self.dev)?;
        let info = bays::read_info(&mut self.dev, placed.info[set as usize])
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("bay{set} holds no record for {slot}"))?;
        Ok((set, info))
    }

    /// Installs a slot's payloads into its bay (see [`bays::stage`]): the set that already holds
    /// the slot, else a set no pointer names. No pointer moves.
    pub fn install_slot(&mut self, slot: &str, payloads: &[&[u8]]) -> Result<bays::Done, String> {
        let layout = self.layout.clone()?;
        let protected = bays::Protected::from_volume(&mut self.dev).map_err(|e| e.to_string())?;
        let set = bays::choose_set(&mut self.dev, slot, &protected).map_err(|e| e.to_string())?;
        bays::stage_with(&mut self.dev, &layout, set, slot, payloads, &protected)
            .map_err(|e| e.to_string())
    }

    /// Frees the bay of `slot` by its record alone (see [`bays::free_set`]); nothing else is written.
    pub fn free_slot(&mut self, slot: &str) -> Result<(), String> {
        self.layout.clone()?;
        let Some(set) = bays::set_of_slot(&mut self.dev, slot).map_err(|e| e.to_string())? else {
            return Ok(());
        };
        // The rule (M36b): `previous.txt` names the default's bay until a real previous exists, and
        // never an empty one. Removing the previous slot's bay therefore points `previous.txt` at the
        // default's bay first (one sector pair), then frees the old bay. A cut between the two leaves
        // the old previous, which still holds its record. Removing the default's bay is refused below.
        let default_set = self.pointer_set(Pointer::Default)?;
        let previous_set = self.pointer_set(Pointer::Previous)?;
        if previous_set == Some(set) && default_set != Some(set) {
            let Some(fallback) = default_set else {
                return Err(format!(
                    "{slot} is the previous slot and the default names no bay: previous.txt has nowhere to fall back to, so {slot} is kept"
                ));
            };
            self.pointers
                .write(&mut self.dev, Pointer::Previous, &bays::set_name(fallback))
                .map_err(|e| e.to_string())?;
        }
        let protected = bays::Protected::from_volume(&mut self.dev).map_err(|e| e.to_string())?;
        bays::free_set(&mut self.dev, set, &protected).map_err(|e| e.to_string())?;
        debug_assert!(
            self.pointer_set(Pointer::Previous)?.is_some(),
            "previous.txt names no bay after a removal"
        );
        Ok(())
    }

    /// The bay set a pointer names, `None` when it is torn or names no set.
    fn pointer_set(&mut self, which: Pointer) -> Result<Option<u8>, String> {
        Ok(self
            .pointers
            .read(&mut self.dev, which)
            .map_err(|e| e.to_string())?
            .as_deref()
            .and_then(bays::set_of))
    }
}

/// A boot partition's payload bays with no pointers, and no record but the reason's: the U-Boot form's volume, whose
/// default is the environment's. It reads a slot's bay and installs and frees a slot the same way
/// [`Boot`] does; it never names a pointer.
pub struct BayVolume<D = FileSectors> {
    dev: D,
    layout: bays::Layout,
    /// The volume's one record, `reason.rec` (the contract lists it for the U-Boot form), when the
    /// image has it.
    reason: Option<Pair>,
}

impl BayVolume<FileSectors> {
    /// Opens the node read-write and reads its bay layout. Nothing is mounted.
    pub fn open(node: &Path) -> Result<Self, String> {
        let file =
            crate::boot_node::open(node, true).map_err(|e| format!("{}: {e}", node.display()))?;
        BayVolume::on(FileSectors::new(file, 0)).map_err(|e| format!("{}: {e}", node.display()))
    }

    /// Opens the node read-only and reads its bay layout: the reads (a slot's bay, the layout) of a daemon
    /// whose root phase handed the boot partition over read-only. Nothing is mounted.
    pub fn open_read(node: &Path) -> Result<Self, String> {
        let file =
            crate::boot_node::open(node, false).map_err(|e| format!("{}: {e}", node.display()))?;
        BayVolume::on(FileSectors::new(file, 0)).map_err(|e| format!("{}: {e}", node.display()))
    }
}

impl<D: Sectors + ReadAt> BayVolume<D> {
    /// The volume on `dev` (a node, or an image in a test): its bay layout, read from its member
    /// list and its sets' records.
    pub fn on(mut dev: D) -> Result<Self, String> {
        let layout = bays::discover(&mut dev)?;
        let reason = bootfat::locate(&mut dev, REASON_FILE, RECORD_BYTES)
            .ok()
            .map(|file| Pair::at(file.first_lba));
        Ok(Self {
            dev,
            layout,
            reason,
        })
    }

    /// The device the volume is read and written through.
    pub fn into_device(self) -> D {
        self.dev
    }

    /// The bay layout the volume was read with: the role sizes, the firmware overlays and the config
    /// digest.
    pub fn bay_layout(&self) -> bays::Layout {
        self.layout.clone()
    }

    /// The bay a slot's files are in (see [`bays::slot_bay_on`]).
    pub fn slot_bay(&mut self, slot: &str) -> Result<(String, Vec<(String, u64)>), String> {
        bays::slot_bay_on(&mut self.dev, &self.layout, slot)
    }

    /// Installs a slot's payloads into its bay (see [`bays::stage_with`]); the set's composed
    /// `extlinux.conf` is written with them. `protected` is the environment's default, previous and
    /// trial (the caller reads them); no pointer or environment variable moves.
    pub fn install_slot_with(
        &mut self,
        slot: &str,
        payloads: &[&[u8]],
        protected: &bays::Protected,
    ) -> Result<bays::Done, String> {
        let set = bays::choose_set(&mut self.dev, slot, protected).map_err(|e| e.to_string())?;
        bays::stage_with(&mut self.dev, &self.layout, set, slot, payloads, protected)
            .map_err(|e| e.to_string())
    }

    /// Frees the bay of `slot` by its record alone (see [`bays::free_set`]). `protected` as for
    /// [`BayVolume::install_slot_with`].
    pub fn free_slot(&mut self, slot: &str, protected: &bays::Protected) -> Result<(), String> {
        let Some(set) = bays::set_of_slot(&mut self.dev, slot).map_err(|e| e.to_string())? else {
            return Ok(());
        };
        bays::free_set(&mut self.dev, set, protected).map_err(|e| e.to_string())
    }
}

/// One install's payloads on the boot partition, written a step a call (see
/// [`Boot::into_payloads`] and [`BayVolume::into_payloads_with`]). It owns the partition's device,
/// so the cursor and the open file live here between the driver's ticks.
pub struct Payloads {
    dev: FileSectors,
    run: Option<bays::SetRun>,
}

impl Payloads {
    /// One step (at most [`bays::STEP_SECTORS`] sectors). `Ok(true)` once the payloads are
    /// committed; `Ok(false)` while steps remain or a flush is in flight.
    pub fn step(&mut self) -> Result<bool, String> {
        Ok(self.step_sectors(bays::STEP_SECTORS)? == bays::Step::Done)
    }

    /// One step of at most `sectors` bulk sectors: [`bays::Step::Done`] once the payloads are
    /// committed, [`bays::Step::Wait`] while a flush is in flight (the next call polls it), else
    /// [`bays::Step::More`]. The driver calls it in pieces until its tick's budget is spent.
    pub fn step_sectors(&mut self, sectors: u64) -> Result<bays::Step, String> {
        let Some(run) = self.run.as_mut() else {
            return Ok(bays::Step::Done);
        };
        let step = run
            .step(&mut self.dev, sectors)
            .map_err(|e| e.to_string())?;
        if step == bays::Step::Done {
            self.run = None;
        }
        Ok(step)
    }
}

impl Boot<FileSectors> {
    /// Starts writing `payloads` (the five roles, then the overlays) into a free bay as `slot`.
    /// The checks run here, as [`Boot::install_slot`] runs them, and refusals come before any write.
    /// The writes then run through [`Payloads::step`].
    pub fn into_payloads(self, slot: &str, payloads: &[Vec<u8>]) -> Result<Payloads, String> {
        let mut boot = self;
        let layout = boot.layout.clone()?;
        let protected = bays::Protected::from_volume(&mut boot.dev).map_err(|e| e.to_string())?;
        let set = bays::choose_set(&mut boot.dev, slot, &protected).map_err(|e| e.to_string())?;
        let refs: Vec<&[u8]> = payloads.iter().map(Vec::as_slice).collect();
        let run = match bays::begin_stage_with(&mut boot.dev, &layout, set, slot, &refs, &protected)
            .map_err(|e| e.to_string())?
        {
            bays::Begun::Finished(_) => None,
            bays::Begun::Writing(run) => Some(*run),
        };
        Ok(Payloads { dev: boot.dev, run })
    }
}

impl BayVolume<FileSectors> {
    /// [`BayVolume::install_slot_with`], with the writes left to step through [`Payloads`]: the
    /// checks and the refusals come here, before any write.
    pub fn into_payloads_with(
        self,
        slot: &str,
        payloads: &[Vec<u8>],
        protected: &bays::Protected,
    ) -> Result<Payloads, String> {
        let mut vol = self;
        let set = bays::choose_set(&mut vol.dev, slot, protected).map_err(|e| e.to_string())?;
        let refs: Vec<&[u8]> = payloads.iter().map(Vec::as_slice).collect();
        let run =
            match bays::begin_stage_with(&mut vol.dev, &vol.layout, set, slot, &refs, protected)
                .map_err(|e| e.to_string())?
            {
                bays::Begun::Finished(_) => None,
                bays::Begun::Writing(run) => Some(*run),
            };
        Ok(Payloads { dev: vol.dev, run })
    }
}

impl<D: Sectors + ReadAt> BootRaw for Boot<D> {
    fn pointer(&mut self, which: Pointer) -> Result<Option<String>, String> {
        self.slot_of_pointer(which)
    }

    /// Points `which` at the slot's bay. The default pointer switch writes the previous pointer first
    /// (the old default's bay, when the default changes), then the default: a cut between the two
    /// leaves the old default and an old previous, both of which hold their records.
    fn set_pointer(&mut self, which: Pointer, slot: &str) -> Result<(), String> {
        let set = self.set_for(slot)?;
        if which == Pointer::Default {
            let old = self
                .pointers
                .read(&mut self.dev, Pointer::Default)
                .map_err(|e| e.to_string())?;
            if let Some(old) = old.as_deref().and_then(bays::set_of)
                && old != set
            {
                self.pointers
                    .write(&mut self.dev, Pointer::Previous, &bays::set_name(old))
                    .map_err(|e| e.to_string())?;
            }
        }
        self.pointers
            .write(&mut self.dev, which, &bays::set_name(set))
            .map_err(|e| e.to_string())
    }

    fn slot_bay(&mut self, slot: &str) -> Result<(String, Vec<(String, u64)>), String> {
        let layout = self.layout.clone()?;
        bays::slot_bay_on(&mut self.dev, &layout, slot)
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
        let set = self.set_for(slot)?;
        self.pointers
            .write(&mut self.dev, Pointer::Trial, &bays::set_name(set))
            .map_err(|e| e.to_string())?;
        self.records
            .set_trial(&mut self.dev, slot, attempts)
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

    fn baseline(&mut self) -> Result<Option<u32>, String> {
        let Some(pair) = self.baseline else {
            return Err("this boot partition has no restart baseline record (count.rec)".into());
        };
        let current = pair
            .read(&mut self.dev, Kind::BASELINE)
            .map_err(|e| e.to_string())?;
        Ok(current.and_then(|cur| decode_baseline(&cur.payload)))
    }

    fn set_baseline(&mut self, count: u32) -> Result<(), String> {
        let Some(pair) = self.baseline else {
            return Err("this boot partition has no restart baseline record (count.rec)".into());
        };
        pair.write(
            &mut self.dev,
            Kind::BASELINE,
            encode_baseline(count).as_bytes(),
        )
        .map(drop)
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

/// What the U-Boot form's volume answers for the records it does not have: its trial and default are
/// in U-Boot's environment.
fn in_the_environment(what: &str) -> String {
    format!(
        "the {what} is in U-Boot's environment on this boot form: the boot partition has no record of it"
    )
}

impl<D: Sectors + ReadAt> BootRaw for BayVolume<D> {
    fn pointer(&mut self, _which: Pointer) -> Result<Option<String>, String> {
        Err(in_the_environment("default"))
    }

    fn set_pointer(&mut self, _which: Pointer, _slot: &str) -> Result<(), String> {
        Err(in_the_environment("default"))
    }

    fn trial(&mut self) -> Result<Option<Trial>, String> {
        Err(in_the_environment("trial"))
    }

    fn set_trial(&mut self, _slot: &str, _attempts: u8) -> Result<(), String> {
        Err(in_the_environment("trial"))
    }

    fn clear_trial(&mut self) -> Result<(), String> {
        Err(in_the_environment("trial"))
    }

    fn reason(&mut self) -> Result<Option<String>, String> {
        let pair = self
            .reason
            .ok_or("reason.rec is missing from this volume")?;
        let Some(cur) = pair
            .read(&mut self.dev, Kind::REASON)
            .map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        Ok((!cur.payload.is_empty()).then(|| String::from_utf8_lossy(&cur.payload).into_owned()))
    }

    fn set_reason(&mut self, reason: &str) -> Result<(), String> {
        let pair = self
            .reason
            .ok_or("reason.rec is missing from this volume")?;
        let text = reason.replace(['\n', '\r'], " ");
        pair.write(&mut self.dev, Kind::REASON, text.as_bytes())
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn episodes(&mut self) -> Result<Option<(String, u8)>, String> {
        Err(no_episodes_here())
    }

    fn set_episodes(&mut self, _slot: &str, _episodes: u8) -> Result<(), String> {
        Err(no_episodes_here())
    }

    fn clear_episodes(&mut self) -> Result<(), String> {
        Err(no_episodes_here())
    }

    fn slot_bay(&mut self, slot: &str) -> Result<(String, Vec<(String, u64)>), String> {
        bays::slot_bay_on(&mut self.dev, &self.layout, slot)
    }
}

fn no_episodes_here() -> String {
    "the episodes are kept on the Pi firmware form's records, which this boot form doesn't have"
        .into()
}

/// The boot partition's records, opened on its node for either form: the Pi firmware form (a trial
/// record is there) strictly, with every record and pointer; the U-Boot form with its reason record
/// alone. Refused by name when the volume is neither.
pub fn open_records(node: &Path) -> Result<Box<dyn BootRaw>, String> {
    let file =
        crate::boot_node::open(node, false).map_err(|e| format!("{}: {e}", node.display()))?;
    let mut probe = FileSectors::new(file, 0);
    if bootfat::locate(&mut probe, TRIAL_FILE, RECORD_BYTES).is_ok() {
        return Boot::open(node).map(|boot| Box::new(boot) as Box<dyn BootRaw>);
    }
    BayVolume::open(node).map(|volume| Box::new(volume) as Box<dyn BootRaw>)
}

#[cfg(test)]
#[path = "boot_raw_tests.rs"]
mod tests;
