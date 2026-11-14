//! Payload bays (I5i8): a slot's boot files are pre-created, fixed-size, contiguous files in one of
//! three bay sets on the boot partition, `slots/bay0/` to `slots/bay2/`. A set is the directory a
//! pointer names (`os_prefix=slots/bayN/`), so the firmware reads the set's files under their fixed
//! names. An install writes a set's file bytes in place, one raw sector at a time, verifies them by
//! hash, and commits by one two-copy record (`bayinfo.rec`) in the set. Only then does the pointer
//! switch make the set live. Nothing here writes filesystem metadata: every write is a sector inside
//! a file that already exists at its fixed size.
//!
//! Order of an install (each step durable before the next; a cut leaves the old state or the new):
//! 1. refuse, before any write, when the slot is on trial, when the set is the default's or the
//!    previous's bay, or when a payload is larger than its bay (the answer names the reason);
//! 2. when the set still holds an old committed record, clear it (one record write), then write each
//!    payload file's sectors, padded to its fixed size;
//! 3. read each file back and compare its hash with the hash of the payload;
//! 4. commit the set's `bayinfo.rec` (the slot and the hashes), read it back;
//! 5. point `previous.txt` at the old default's set, then `default.txt` at the new set. A pointer is
//!    two sectors holding the same `os_prefix` line; the second sector is written first, so at every
//!    cut at least one sector holds a whole line, and the last whole line in the file is the one read.
//!
//! The firmware parses the pointer text itself; what it does with a damaged sector is not known from
//! its documentation, and is listed as a board check in `testing/board-runs/I5i8.md`.

use crate::boot_raw::{self, Trial};
use crate::bootfat::{self, ReadAt, Refusal};
use crate::firmware;
use crate::twocopy::{Kind, Pair, SECTOR, Sectors};
use sha2::{Digest, Sha256};

/// The number of bay sets: the default, the previous, and the one being installed or on trial.
pub const SETS: u8 = 3;
/// The per-set record, two sectors (`bayinfo.rec`).
pub const INFO_NAME: &str = "bayinfo.rec";
/// Its size on the partition.
pub const INFO_BYTES: u64 = 1024;
/// The pointer files at the partition's root: two sectors each ([`firmware::POINTER_FILE_BYTES`]).
pub const POINTER_BYTES: u64 = firmware::POINTER_FILE_BYTES as u64;
/// The default and previous pointers.
pub const DEFAULT_POINTER: &str = "default.txt";
/// The previous pointer.
pub const PREVIOUS_POINTER: &str = "previous.txt";
/// The trial pointer (`trial.txt`), which `tryboot.txt` includes.
pub const TRIAL_POINTER: &str = "trial.txt";
/// The trial record (the pending trial), two sectors, at the partition's root.
pub const TRIAL_RECORD: &str = "trial.rec";

/// One payload file of a bay set: its role, its path under the set, and the byte that pads it to
/// its fixed size (`cmdline.txt` is padded with spaces, so the firmware's text parser sees blanks).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Role {
    /// The build role (`kernel`, `initramfs`, `device-tree`, `cmdline`, `board-overlay`).
    pub role: &'static str,
    /// The file's path under the set directory: the name the firmware reads.
    pub path: &'static str,
    /// The padding byte.
    pub pad: u8,
}

/// The five payload roles, in the order the install takes them. Version 5 of the layout contract
/// keeps these names.
pub const ROLES: [Role; 5] = [
    Role {
        role: "kernel",
        path: "kernel8.img",
        pad: 0,
    },
    Role {
        role: "initramfs",
        path: "initramfs.cpio",
        pad: 0,
    },
    Role {
        role: "device-tree",
        path: "board.dtb",
        pad: 0,
    },
    Role {
        role: "cmdline",
        path: "cmdline.txt",
        pad: b' ',
    },
    Role {
        role: "board-overlay",
        path: "overlays/lemnos-board.dtbo",
        pad: 0,
    },
];

/// The files a slot has on the boot partition, on every form: the role files' paths, in [`ROLES`]
/// order. The install, the image builder and the every-boot check all name a slot's files by this
/// list, so a slot that installs is a slot whose boot check can find every file it lists.
pub fn stored_files() -> [&'static str; 5] {
    std::array::from_fn(|i| ROLES[i].path)
}

/// The `boot.files` entries of a manifest that no bay stores, in the manifest's order.
pub fn unstored(boot_files: &[(String, String)]) -> Vec<&str> {
    boot_files
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| !ROLES.iter().any(|r| r.path == *name))
        .collect()
}

/// The refusal for a manifest whose `boot.files` lists a file no bay stores, or leaves a role file
/// out; `Ok` when the list is exactly the role files. The install makes it before any write: a slot that lists such a file would install
/// and then fail its own boot check, which finds the file missing.
pub fn refuse_unstored(slot: &str, boot_files: &[(String, String)]) -> Result<(), String> {
    let extra = unstored(boot_files);
    if extra.is_empty() {
        // The reverse: a role file the manifest does not list is never hashed, so it is refused too.
        if let Some(unlisted) = stored_files()
            .into_iter()
            .find(|path| !boot_files.iter().any(|(name, _)| name == path))
        {
            return Err(format!(
                "{slot}: boot.files does not list {unlisted:?}, which this board's boot partition stores (it stores: {}): nothing was written, the default keeps booting",
                stored_files().join(", ")
            ));
        }
        return Ok(());
    }
    let listed: Vec<String> = extra.iter().map(|name| format!("{name:?}")).collect();
    Err(format!(
        "{slot}: boot.files lists {}, which this board's boot partition does not store (it stores: {}): nothing was written, the default keeps booting",
        listed.join(", "),
        stored_files().join(", ")
    ))
}

/// The fixed size of each role's file, in roles order. A multiple of the sector size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Bytes per role.
    pub bytes: [u64; 5],
}

impl Layout {
    /// A layout, refused by name when a size is zero or not a whole number of sectors.
    pub fn new(bytes: [u64; 5]) -> Result<Self, String> {
        for (r, &b) in ROLES.iter().zip(bytes.iter()) {
            if b == 0 || b % SECTOR as u64 != 0 {
                return Err(format!(
                    "bay for {}: {b} bytes is not a whole number of {SECTOR}-byte sectors",
                    r.role
                ));
            }
        }
        Ok(Self { bytes })
    }
}

/// The set directory's name: `slots/bayN`.
pub fn set_dir(set: u8) -> String {
    format!("slots/bay{set}")
}

/// The name a pointer gives a set (`bayN`), the text its `os_prefix` line carries.
pub fn set_name(set: u8) -> String {
    format!("bay{set}")
}

/// The set a pointer names (`bayN`), `None` for any other name.
pub fn set_of(name: &str) -> Option<u8> {
    let n: u8 = name.strip_prefix("bay")?.parse().ok()?;
    (n < SETS).then_some(n)
}

/// What an install did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Done {
    /// The payloads are in the set, committed, and the pointers name it.
    Installed,
    /// The set already is the default and holds this slot: nothing was written (the re-run after an
    /// interrupted install).
    AlreadyInstalled,
}

/// Why an install did not finish.
#[derive(Debug)]
pub enum Error {
    /// Refused before any write, or stopped before a commit: the answer, with its reason. What the
    /// boot partition holds is the old state (a cut may have written payload sectors into a set
    /// that no pointer names).
    Refused(String),
    /// The device failed, or a read-back did not match what was written. Nothing was committed.
    Device(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Refused(why) | Error::Device(why) => f.write_str(why),
        }
    }
}

fn io_err(e: std::io::Error) -> Error {
    Error::Device(e.to_string())
}

fn sha(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// A set's record: the slot installed in it, and each payload file's length and hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    /// The slot id.
    pub slot: String,
    /// Each role's stored length and hash, in [`ROLES`] order.
    pub files: [(u64, [u8; 32]); 5],
}

impl Info {
    /// Each payload's path under its set (the name the manifest lists) and its stored length.
    pub fn lengths(&self) -> Vec<(String, u64)> {
        ROLES
            .iter()
            .zip(self.files.iter())
            .map(|(r, (len, _))| (r.path.to_string(), *len))
            .collect()
    }

    /// The record's payload bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![self.slot.len() as u8];
        out.extend_from_slice(self.slot.as_bytes());
        for (len, hash) in &self.files {
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(hash);
        }
        out
    }

    /// The record from its payload bytes; `None` for any other shape.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (&n, rest) = bytes.split_first()?;
        let n = n as usize;
        let slot = std::str::from_utf8(rest.get(..n)?).ok()?.to_string();
        if !crate::store::valid_id(&slot) {
            return None;
        }
        let mut at = 1 + n;
        let mut files = [(0u64, [0u8; 32]); 5];
        for f in files.iter_mut() {
            let len = u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?);
            let hash: [u8; 32] = bytes.get(at + 8..at + 40)?.try_into().ok()?;
            *f = (len, hash);
            at += 40;
        }
        (at == bytes.len()).then_some(Self { slot, files })
    }
}

/// Reads the pointer file's effective set: the last whole line of its two sectors (the one form,
/// [`firmware::pointer_file_slot`]). `None` when neither sector holds a whole line.
pub fn read_pointer<D: Sectors>(dev: &mut D, lba: u64) -> Result<Option<u8>, Error> {
    let mut file = [0u8; POINTER_BYTES as usize];
    for (k, chunk) in file.chunks_mut(SECTOR).enumerate() {
        dev.read(lba + k as u64, chunk.try_into().expect("a whole sector"))
            .map_err(io_err)?;
    }
    Ok(firmware::pointer_file_slot(&file).and_then(|name| set_of(&name)))
}

/// Writes the pointer file to name `set`: the second sector, then the first.
pub fn write_pointer<D: Sectors>(dev: &mut D, lba: u64, set: u8) -> Result<(), Error> {
    let file = firmware::pointer_file(&format!("bay{set}")).expect("bay names are valid ids");
    for k in [1usize, 0] {
        let at = k * SECTOR;
        dev.write(
            lba + k as u64,
            file[at..at + SECTOR].try_into().expect("a whole sector"),
        )
        .map_err(io_err)?;
    }
    Ok(())
}

/// The partition's placed files the install and the boot check need.
pub struct Placed {
    /// Each role's first sector, per set.
    pub files: Vec<[u64; 5]>,
    /// Each set's record, first sector.
    pub info: Vec<u64>,
    /// The default pointer's first sector.
    pub default: u64,
    /// The previous pointer's first sector.
    pub previous: u64,
    /// The trial record's first sector.
    pub trial: u64,
    /// The trial pointer's first sector (`trial.txt`).
    pub trial_pointer: u64,
}

/// The bays the caller says are protected. A bay named here is never written or freed, and an install
/// is refused while `pending` is set. The Pi firmware form reads them from its pointer and trial
/// files ([`Protected::from_volume`]); the U-Boot form keeps its default, previous and trial in the
/// environment, so its caller supplies them. The bay operations never look for them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Protected {
    /// The set the default pointer names.
    pub default: Option<u8>,
    /// The set the previous pointer names.
    pub previous: Option<u8>,
    /// The set the trial names (the trial pointer on the Pi form).
    pub trial: Option<u8>,
    /// A trial with attempts left: nothing is installed while it is pending.
    pub pending: Option<Trial>,
}

impl Protected {
    /// The Pi firmware form's protected sets, read from the volume's pointer files and trial record.
    pub fn from_volume<D: ReadAt + Sectors>(dev: &mut D, layout: &Layout) -> Result<Self, Error> {
        let placed = place(dev, layout).map_err(Error::Refused)?;
        let pending = pending_trial(dev, placed.trial)?;
        // The trial pointer names a bay only while a trial record exists: arming writes the pointer
        // first, and the pointer stays after the record is cleared, so a stale one protects nothing
        // (it would otherwise lock its bay for good, and the image builder points it at set 0).
        let trial = match pending {
            Some(_) => read_pointer(dev, placed.trial_pointer)?,
            None => None,
        };
        Ok(Self {
            default: read_pointer(dev, placed.default)?,
            previous: read_pointer(dev, placed.previous)?,
            trial,
            pending,
        })
    }

    /// Whether `set` is named by any protected entry.
    pub fn names(&self, set: u8) -> bool {
        [self.default, self.previous, self.trial].contains(&Some(set))
    }
}

/// Locates every bay file, record and pointer, reading the volume only. A file that is missing,
/// fragmented or the wrong size is refused by name, and nothing is written.
pub fn place<D: ReadAt>(dev: &mut D, layout: &Layout) -> Result<Placed, String> {
    let sets = place_sets(dev, layout)?;
    let files = sets.iter().map(|(f, _)| *f).collect();
    let info = sets.iter().map(|(_, i)| *i).collect();
    let mut at = |path: String, want: u64| -> Result<u64, String> {
        bootfat::locate_path(dev, &path, want)
            .map(|f| f.first_lba)
            .map_err(|e: Refusal| format!("{path}: {e}"))
    };
    Ok(Placed {
        files,
        info,
        default: at(DEFAULT_POINTER.into(), POINTER_BYTES)?,
        previous: at(PREVIOUS_POINTER.into(), POINTER_BYTES)?,
        trial: at(TRIAL_RECORD.into(), INFO_BYTES)?,
        trial_pointer: at(TRIAL_POINTER.into(), POINTER_BYTES)?,
    })
}

/// Writes `bytes` (padded with `pad` to `size`) into the file at `lba`, one sector at a time.
pub(crate) fn write_file<D: Sectors>(
    dev: &mut D,
    lba: u64,
    bytes: &[u8],
    size: u64,
    pad: u8,
) -> Result<(), Error> {
    let sectors = size / SECTOR as u64;
    for k in 0..sectors {
        let mut s = [pad; SECTOR];
        let from = (k as usize) * SECTOR;
        if from < bytes.len() {
            let to = (from + SECTOR).min(bytes.len());
            s[..to - from].copy_from_slice(&bytes[from..to]);
        }
        dev.write(lba + k, &s).map_err(io_err)?;
    }
    Ok(())
}

/// Reads the first `len` bytes of the file at `lba`.
pub(crate) fn read_file<D: Sectors>(dev: &mut D, lba: u64, len: u64) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(len as usize);
    let sectors = len.div_ceil(SECTOR as u64);
    for k in 0..sectors {
        let mut s = [0u8; SECTOR];
        dev.read(lba + k, &mut s).map_err(io_err)?;
        let take = (len as usize - out.len()).min(SECTOR);
        out.extend_from_slice(&s[..take]);
    }
    Ok(out)
}

/// The current trial, if one is pending: its slot and attempts.
pub fn pending_trial<D: Sectors>(dev: &mut D, lba: u64) -> Result<Option<Trial>, Error> {
    let pair = Pair::at(lba);
    let cur = pair
        .read(dev, Kind::TRIAL)
        .map_err(|e| Error::Device(e.to_string()))?;
    Ok(cur.and_then(|c| boot_raw::decode_trial(&c.payload)))
}

/// The sets' files and records, placed on the volume without the pointers: the U-Boot form keeps
/// its pointer in the environment, so it places only these. Each set's roles, then its record.
pub fn place_sets<D: ReadAt>(dev: &mut D, layout: &Layout) -> Result<Vec<([u64; 5], u64)>, String> {
    let mut out = Vec::new();
    for set in 0..SETS {
        let mut lbas = [0u64; 5];
        for (i, (r, want)) in ROLES.iter().zip(layout.bytes).enumerate() {
            let path = format!("{}/{}", set_dir(set), r.path);
            lbas[i] = bootfat::locate_path(dev, &path, want)
                .map_err(|e: Refusal| format!("{path}: {e}"))?
                .first_lba;
        }
        let path = format!("{}/{INFO_NAME}", set_dir(set));
        let info = bootfat::locate_path(dev, &path, INFO_BYTES)
            .map_err(|e: Refusal| format!("{path}: {e}"))?
            .first_lba;
        out.push((lbas, info));
    }
    Ok(out)
}

/// The argument checks of an install, before any read or write: the set, the slot id, and each
/// payload against its bay. A refusal names the reason and writes nothing.
pub fn check_install(
    layout: &Layout,
    set: u8,
    slot: &str,
    payloads: &[&[u8]; 5],
) -> Result<(), Error> {
    if set >= SETS {
        return Err(Error::Refused(format!(
            "bay set {set} does not exist: there are {SETS}"
        )));
    }
    if !crate::store::valid_id(slot) {
        return Err(Error::Refused(format!("{slot:?} is not a slot id")));
    }
    for (i, r) in ROLES.iter().enumerate() {
        if payloads[i].len() as u64 > layout.bytes[i] {
            return Err(Error::Refused(format!(
                "{slot}: the {} payload is {} bytes but its bay holds {}: nothing was written, \
                 the default keeps booting; rebuild the image with a larger {} bay or a smaller payload",
                r.role,
                payloads[i].len(),
                layout.bytes[i],
                r.role
            )));
        }
    }
    Ok(())
}

/// Steps 2 to 4 of an install, for one set: the payload bytes in place, a read-back of each by
/// hash, then the set's record (the commit), read back. Nothing a pointer names is touched.
pub fn commit_set<D: Sectors>(
    dev: &mut D,
    layout: &Layout,
    files: &[u64; 5],
    info: u64,
    slot: &str,
    payloads: &[&[u8]; 5],
) -> Result<(), Error> {
    commit_set_with(dev, layout, files, info, slot, payloads, None)
}

/// [`commit_set`] with one more generated file written before the record: the U-Boot form's
/// `extlinux.conf`, composed from the command line (`(first sector, text, bay bytes)`). Its bytes are
/// not hashed in the record; the boot check composes the same text from the stored command line.
pub fn commit_set_with<D: Sectors>(
    dev: &mut D,
    layout: &Layout,
    files: &[u64; 5],
    info: u64,
    slot: &str,
    payloads: &[&[u8]; 5],
    extra: Option<(u64, &[u8], u64)>,
) -> Result<(), Error> {
    for (i, r) in ROLES.iter().enumerate() {
        write_file(dev, files[i], payloads[i], layout.bytes[i], r.pad)?;
    }
    if let Some((lba, text, bytes)) = extra {
        write_file(dev, lba, text, bytes, b' ')?;
    }
    let mut stored = [(0u64, [0u8; 32]); 5];
    for (i, p) in payloads.iter().enumerate() {
        let back = read_file(dev, files[i], p.len() as u64)?;
        let want = sha(p);
        if back.len() != p.len() || sha(&back) != want {
            return Err(Error::Device(format!(
                "{slot}: the {} read-back does not match what was written: nothing was committed",
                ROLES[i].role
            )));
        }
        stored[i] = (p.len() as u64, want);
    }
    let record = Info {
        slot: slot.to_string(),
        files: stored,
    };
    Pair::at(info)
        .write(dev, Kind::BAY_INFO, &record.encode())
        .map_err(|e| Error::Device(e.to_string()))?;
    if read_info(dev, info)?.as_ref() != Some(&record) {
        return Err(Error::Device(format!(
            "{slot}: the set's record did not read back"
        )));
    }
    Ok(())
}

/// Installs `payloads` (in [`ROLES`] order) as slot `slot` into bay set `set`, and makes it the
/// default; the previous default becomes the previous. [`stage`] then [`make_default`]: a cut
/// between the two leaves the old pointers, which name their own sets, untouched.
pub fn install<D: Sectors + ReadAt>(
    dev: &mut D,
    layout: &Layout,
    set: u8,
    slot: &str,
    payloads: &[&[u8]; 5],
) -> Result<Done, Error> {
    let protected = Protected::from_volume(dev, layout)?;
    let done = stage_with(dev, layout, set, slot, payloads, None, &protected)?;
    if done == Done::Installed {
        make_default(dev, layout, set, &protected)?;
    }
    Ok(done)
}

/// The install's write part, which moves no pointer: the refusals, then the payloads in place, their
/// read-back by hash, and the set's record (the commit). A default set that holds no committed
/// record takes the install in place: nothing bootable is in it, so nothing bootable can be lost.
pub fn stage<D: Sectors + ReadAt>(
    dev: &mut D,
    layout: &Layout,
    set: u8,
    slot: &str,
    payloads: &[&[u8]; 5],
) -> Result<Done, Error> {
    let protected = Protected::from_volume(dev, layout)?;
    stage_with(dev, layout, set, slot, payloads, None, &protected)
}

/// [`stage`] with the caller's protected sets, and for a set that may carry the U-Boot form's composed
/// `extlinux/extlinux.conf`: `text` is written into the set's fixed-size extlinux file with the
/// payloads, before the record. A default set with no committed record takes the install in place (as
/// before); every other protected bay is refused.
pub fn stage_with<D: Sectors + ReadAt>(
    dev: &mut D,
    layout: &Layout,
    set: u8,
    slot: &str,
    payloads: &[&[u8]; 5],
    text: Option<&[u8]>,
    protected: &Protected,
) -> Result<Done, Error> {
    check_install(layout, set, slot, payloads)?;
    if let Some(t) = &protected.pending {
        return Err(Error::Refused(format!(
            "slot {} is on trial with {} attempts left: nothing was written; wait until the trial \
             is promoted or falls back, then install {slot} again",
            t.slot, t.attempts
        )));
    }
    let sets = place_sets(dev, layout).map_err(Error::Refused)?;
    let (files, info) = sets[set as usize];
    if protected.default == Some(set) {
        match read_info(dev, info)? {
            Some(i) if i.slot == slot => return Ok(Done::AlreadyInstalled),
            Some(_) => {
                return Err(Error::Refused(format!(
                    "bay{set} is the default and does not hold {slot}: nothing was written"
                )));
            }
            None => {}
        }
    } else if protected.previous == Some(set) {
        return Err(Error::Refused(format!(
            "bay{set} is the previous slot's bay: a new install takes one of the other bays"
        )));
    } else if protected.trial == Some(set) {
        return Err(Error::Refused(format!(
            "bay{set} is the trial's bay: a new install takes one of the other bays"
        )));
    }
    let extra = match text {
        None => None,
        Some(text) => {
            let max = crate::bootimage::EXTLINUX_BYTES;
            if text.len() as u64 > max {
                return Err(Error::Refused(format!(
                    "{slot}: the composed extlinux.conf is {} bytes, more than its bay's {max}: nothing was written",
                    text.len()
                )));
            }
            let path = format!("{}/{}", set_dir(set), crate::bootimage::EXTLINUX);
            let found = bootfat::locate_path(dev, &path, max)
                .map_err(|e| Error::Refused(format!("{path}: {e}")))?;
            Some((found.first_lba, text, max))
        }
    };
    // A set that still holds a committed record (an old slot's, no longer named by anything) is
    // cleared first, by one record write: a cut during the payload writes then leaves an empty set,
    // never a record whose files no longer match. A cut during the clear leaves the old record over
    // the old files, which no payload write has touched yet.
    if read_info(dev, info)?.is_some() {
        Pair::at(info)
            .write(dev, Kind::BAY_INFO, &[0])
            .map_err(|e| Error::Device(e.to_string()))?;
    }
    commit_set_with(dev, layout, &files, info, slot, payloads, extra)?;
    Ok(Done::Installed)
}

/// Makes `set` the default and the old default (`protected.default`) the previous: previous first, then
/// default (the pointer switch, step 5). The set must hold a committed record for the slot it is to boot.
pub fn make_default<D: Sectors + ReadAt>(
    dev: &mut D,
    layout: &Layout,
    set: u8,
    protected: &Protected,
) -> Result<(), Error> {
    let sets = place_sets(dev, layout).map_err(Error::Refused)?;
    if read_info(dev, sets[set as usize].1)?.is_none() {
        return Err(Error::Refused(format!(
            "bay{set} holds no committed record: nothing was pointed at it"
        )));
    }
    let placed = place(dev, layout).map_err(Error::Refused)?;
    if let Some(d) = protected.default
        && d != set
    {
        write_pointer(dev, placed.previous, d)?;
    }
    write_pointer(dev, placed.default, set)
}

/// The set that holds `slot`'s committed record, `None` when no set does.
pub fn set_of_slot<D: ReadAt + Sectors>(
    dev: &mut D,
    layout: &Layout,
    slot: &str,
) -> Result<Option<u8>, Error> {
    let sets = place_sets(dev, layout).map_err(Error::Refused)?;
    for set in 0..SETS {
        if let Some(i) = read_info(dev, sets[set as usize].1)?
            && i.slot == slot
        {
            return Ok(Some(set));
        }
    }
    Ok(None)
}

/// The set an install of `slot` takes: the set that already holds it (an install again), else the
/// first set no pointer names (default, previous or trial). Refused by name when every set is named.
pub fn choose_set<D: ReadAt + Sectors>(
    dev: &mut D,
    layout: &Layout,
    slot: &str,
    protected: &Protected,
) -> Result<u8, Error> {
    if let Some(set) = set_of_slot(dev, layout, slot)? {
        return Ok(set);
    }
    (0..SETS).find(|set| !protected.names(*set)).ok_or_else(|| {
        Error::Refused(format!(
            "{slot}: every bay set is named by a pointer: nothing was written"
        ))
    })
}

/// Frees a set by its record alone: the record then holds no slot. The caller has checked that no
/// pointer names the set.
pub fn free_set<D: Sectors + ReadAt>(
    dev: &mut D,
    layout: &Layout,
    set: u8,
    protected: &Protected,
) -> Result<(), Error> {
    if protected.names(set) {
        return Err(Error::Refused(format!(
            "bay{set} is named by a protected entry (default, previous or trial): it is not freed"
        )));
    }
    let sets = place_sets(dev, layout).map_err(Error::Refused)?;
    Pair::at(sets[set as usize].1)
        .write(dev, Kind::BAY_INFO, &[0])
        .map_err(|e| Error::Device(e.to_string()))?;
    Ok(())
}

/// The layout the image was built with, read from the sizes of the first bay set's files and checked
/// against every other set. A set that is missing, or whose sizes differ, is refused by name.
pub fn discover<D: ReadAt>(dev: &mut D) -> Result<Layout, String> {
    let mut bytes = [0u64; 5];
    for (i, r) in ROLES.iter().enumerate() {
        let path = format!("{}/{}", set_dir(0), r.path);
        bytes[i] = bootfat::size_of_path(dev, &path).map_err(|e| format!("{path}: {e}"))?;
    }
    for set in 1..SETS {
        for (i, r) in ROLES.iter().enumerate() {
            let path = format!("{}/{}", set_dir(set), r.path);
            let size = bootfat::size_of_path(dev, &path).map_err(|e| format!("{path}: {e}"))?;
            if size != bytes[i] {
                return Err(format!(
                    "{path}: {size} bytes, but bay0's {} is {} bytes: the sets differ",
                    r.role, bytes[i]
                ));
            }
        }
    }
    Layout::new(bytes)
}

/// The set's record, `None` when neither copy is one.
pub fn read_info<D: Sectors>(dev: &mut D, lba: u64) -> Result<Option<Info>, Error> {
    let cur = Pair::at(lba)
        .read(dev, Kind::BAY_INFO)
        .map_err(|e| Error::Device(e.to_string()))?;
    Ok(cur.and_then(|c| Info::decode(&c.payload)))
}

/// What the boot chain reads from a pointer: the set it names, the slot in that set, and whether each
/// payload file's stored bytes hash to the record's hashes. A mismatch or a missing record is an
/// error with its reason.
pub fn boot_view<D: Sectors + ReadAt>(
    dev: &mut D,
    layout: &Layout,
    pointer_lba: u64,
) -> Result<(u8, String), String> {
    let placed = place(dev, layout)?;
    let set = read_pointer(dev, pointer_lba)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "the pointer holds no whole line".to_string())?;
    let info = read_info(dev, placed.info[set as usize])
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("bay{set} holds no committed record"))?;
    for (i, (len, hash)) in info.files.iter().enumerate() {
        let bytes =
            read_file(dev, placed.files[set as usize][i], *len).map_err(|e| e.to_string())?;
        if sha(&bytes) != *hash {
            return Err(format!(
                "bay{set} ({}): the {} file does not match its record",
                info.slot, ROLES[i].role
            ));
        }
    }
    Ok((set, info.slot))
}

/// The bay a slot's files are in: the set's directory (`slots/bayN`) and each payload's stored length
/// from the set's committed record. Refused by name when no set holds the slot, or its set has no
/// committed record. Shared by the Pi firmware form ([`crate::boot_raw::Boot`]) and the U-Boot form
/// ([`crate::boot_raw::BayVolume`]), whose volumes differ only in their pointers and records.
pub fn slot_bay_on<D: ReadAt + Sectors>(
    dev: &mut D,
    layout: &Layout,
    slot: &str,
) -> Result<(String, Vec<(String, u64)>), String> {
    let set = set_of_slot(dev, layout, slot)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("slot {slot} has no bay on the boot partition: nothing names it"))?;
    let sets = place_sets(dev, layout)?;
    let info = read_info(dev, sets[set as usize].1)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("bay{set} holds no committed record"))?;
    Ok((set_dir(set), info.lengths()))
}

#[cfg(test)]
#[path = "bays_tests.rs"]
mod tests;
