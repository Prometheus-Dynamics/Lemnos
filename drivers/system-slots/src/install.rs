//! An install: a bundle streamed in, unpacked into a work directory as it comes, checked against
//! its signed manifest, and moved into place.

use crate::words;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ed25519_dalek::{Signature, VerifyingKey};
use lemnos_core::{CallerId, JobId, Status};
use lemnos_slot_store::bundle::{self, Member};
use lemnos_slot_store::signing::RECORD_LEN;
use lemnos_slot_store::signing::{self, Check, Outcome, Refusal};
use sha2::{Digest, Sha256};

use crate::boot::{self, hex};
use crate::manifest::Manifest;
use crate::store::{Layout, Meta};
use crate::tar::{Kind, Sink, Tar};
use lemnos_slot_store::ubi::{self, UbiWrite};

/// The length of a bundle's `slot.sig`: the shared signature record (`lemnos_slot_store::signing`),
/// 108 bytes. The bare 64-byte signature of the first version is no longer accepted.
pub const SIGNATURE_LEN: usize = RECORD_LEN;
/// No space left on the device.
pub const NO_SPACE: Status = Status::from_errno(28);
/// What an install is called in its work directory's name.
pub const WORK_PREFIX: &str = ".install.";
/// How many bytes of a member are written before they are flushed to the disk.
const SYNC_EVERY: u64 = 4 << 20;

/// Why an install stopped: the status the job ends with, and words for the board's state.
pub type Failure = (Status, String);

fn bad(text: impl Into<String>) -> Failure {
    (Status::INVALID_ARGUMENT, text.into())
}

fn disk(what: &str) -> impl Fn(io::Error) -> Failure + '_ {
    move |error| (Status::IO, format!("{what}: {error}"))
}

/// A key the board trusts, and whose it is.
#[derive(Debug, Clone)]
pub struct TrustedKey {
    pub key: VerifyingKey,
    /// Who holds the private half, as the key file names them.
    pub owner: String,
}

/// The keys the board trusts: one per line, 64 hex digits, a space, then the key's owner (any
/// text, which `list` shows for a slot that key signed). Blank lines and lines starting with `#`
/// are skipped.
pub fn parse_keys(text: &str) -> Result<Vec<TrustedKey>, String> {
    let mut keys = Vec::new();
    for (at, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (hex, owner) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let owner = owner.trim();
        if owner.is_empty() {
            return Err(format!("line {} of the key file names no owner", at + 1));
        }
        let bytes = unhex(hex)
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
            .ok_or_else(|| format!("line {} of the key file is not 64 hex digits", at + 1))?;
        keys.push(TrustedKey {
            key: VerifyingKey::from_bytes(&bytes)
                .map_err(|_| format!("line {} of the key file is not a key", at + 1))?,
            owner: owner.to_string(),
        });
    }
    Ok(keys)
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

/// The slots a bundle may not replace.
#[derive(Debug, Clone, Default)]
pub struct Guard {
    pub running: String,
    pub default: String,
}

impl Guard {
    fn check(&self, id: &str) -> Result<(), Failure> {
        bundle::check_slot_id(id).map_err(bad)?;
        if id == self.running {
            return Err((Status::BUSY, format!("{id} is the running slot")));
        }
        if id == self.default {
            return Err((Status::BUSY, format!("{id} is the default slot")));
        }
        Ok(())
    }
}

struct Writing {
    file: File,
    hasher: Sha256,
    name: String,
    /// Bytes written since the last flush.
    unsynced: u64,
}

/// Where the image goes while it comes, on raw NAND: the device's writer and the work volume.
#[derive(Clone)]
struct WorkVolume {
    writer: Arc<dyn UbiWrite>,
    name: String,
}

/// The tar's members, into the work directory (and the image into its work volume on raw NAND).
struct Unpack {
    work: PathBuf,
    volume: Option<WorkVolume>,
    guard: Guard,
    /// The board's keys and policy, given when the install started.
    admit: Admit,
    manifest: Vec<u8>,
    signature: Vec<u8>,
    /// slot.json as it parses, once it has come.
    parsed: Option<Manifest>,
    /// Every member begun, in order (directories included).
    entries: Vec<String>,
    /// The signature decision and the board's checks: made once, when the bundle first needs
    /// it (the end of slot.sig, or the first payload member), and the same answer after.
    decided: Option<Result<Outcome, Failure>>,
    /// The decision's words for `info`: the outcome, or the refusal's text.
    signed: Option<Result<Outcome, String>>,
    current: Option<(Member, Option<Writing>)>,
    /// Each file written, with its SHA-256.
    hashes: Vec<(String, String)>,
    failure: Option<Failure>,
    /// The durability barriers started by the stream (every SYNC_EVERY bytes, and at each member's
    /// end). They run on the flush worker, so no call waits on the medium; the check phase waits for
    /// them before anything is committed.
    flushes: Vec<lemnos_slot_store::flush::Barrier>,
}

impl Unpack {
    fn fail<T>(&mut self, failure: Failure) -> Result<T, String> {
        let text = failure.1.clone();
        self.failure.get_or_insert(failure);
        Err(text)
    }

    /// The member's place in the bundle's order, refused by a sentence naming the rule.
    fn order(&mut self, name: &str, kind: Kind) -> Result<Member, Failure> {
        let what = bundle::check_order(&self.entries, name, kind).map_err(bad)?;
        if matches!(what, Member::Root | Member::Boot) {
            // The first payload member: the signature and the board are decided now, so a
            // refused bundle has written nothing and the root image is not consumed.
            self.settle()?;
            if what == Member::Boot && kind == Kind::File {
                let file = name.strip_prefix("boot/").unwrap_or(name);
                if let Some(manifest) = &self.parsed {
                    bundle::check_listed(manifest, file).map_err(bad)?;
                } else {
                    return Err(bad(format!(
                        "boot file {file} isn't in slot.json: a file the manifest does not list is refused"
                    )));
                }
            }
        }
        Ok(what)
    }

    /// The signature decision and the board's checks, made once.
    fn settle(&mut self) -> Result<Outcome, Failure> {
        if self.decided.is_none() {
            let result = self.decide();
            self.signed = Some(match &result {
                Ok(outcome) => Ok(outcome.clone()),
                Err((_, text)) => Err(text.clone()),
            });
            self.decided = Some(result);
        }
        self.decided
            .clone()
            .unwrap_or_else(|| Err(bad("the signature was not decided")))
    }

    fn decide(&self) -> Result<Outcome, Failure> {
        let check = signature_check(&self.admit.keys, &self.manifest, &self.signature);
        let outcome = match signing::decide(self.admit.require_signed, "the bundle", check) {
            Ok(outcome) => outcome,
            Err(Refusal::Malformed(text)) => return Err(bad(text)),
            Err(Refusal::Policy(text)) => return Err((Status::NOT_PERMITTED, text)),
        };
        if self.admit.board_id.is_empty() {
            return Err((
                Status::NO_DEVICE,
                "the board file has no [board] id to match a slot's board against".into(),
            ));
        }
        let Some(parsed) = &self.parsed else {
            return Err(bad("slot.json isn't a slot manifest"));
        };
        if parsed.board != self.admit.board_id {
            return Err((
                Status::NOT_SUPPORTED,
                format!(
                    "this slot is built for board {}, and this is board {}",
                    parsed.board, self.admit.board_id
                ),
            ));
        }
        if let Some(why) =
            crate::medium::mismatch(&parsed.id, &parsed.medium, &self.admit.booted_medium)
        {
            return Err((
                Status::NOT_SUPPORTED,
                format!("{why}: a slot for another medium is refused, and nothing was written"),
            ));
        }
        // The boot partition stores the role files and nothing else, on every form: a slot that
        // lists another would install and then fail its own boot check. Refused here, before the
        // first payload byte, so nothing is written.
        lemnos_slot_store::bundle::check_declared(
            parsed,
            &self.admit.firmware,
            self.admit.config.as_deref(),
        )
        .map_err(bad)?;
        Ok(outcome)
    }
}

impl Unpack {
    /// A durability barrier for `file` on the flush worker: `sync_all` (data and metadata) when `all`,
    /// else `sync_data`. Started here, waited for by [`Unpack::flushed`].
    fn barrier(all: bool, file: &File) -> io::Result<lemnos_slot_store::flush::Barrier> {
        let dup = file.try_clone()?;
        lemnos_slot_store::flush::Barrier::start(Box::new(move || {
            if all { dup.sync_all() } else { dup.sync_data() }
        }))
    }

    /// Whether every barrier the stream started has finished. A failed one is the disk going wrong.
    fn flushed(&mut self) -> Result<bool, Failure> {
        let mut left = Vec::with_capacity(self.flushes.len());
        for mut barrier in self.flushes.drain(..) {
            if !barrier.poll().map_err(disk("the work directory"))? {
                left.push(barrier);
            }
        }
        self.flushes = left;
        Ok(self.flushes.is_empty())
    }
}

impl Sink for Unpack {
    fn begin(&mut self, name: &str, kind: Kind, size: u64) -> Result<(), String> {
        let what = match self.order(name, kind) {
            Ok(what) => what,
            Err(failure) => return self.fail(failure),
        };
        self.entries.push(name.to_string());
        match (what, kind) {
            (Member::Boot, Kind::Dir) => {
                self.current = Some((what, None));
                fs::create_dir_all(self.work.join(name)).map_err(|e| e.to_string())
            }
            (_, Kind::Dir) => self.fail(bad(format!("{name} is a directory in the bundle"))),
            (Member::Manifest | Member::Signature, _) => {
                if let Err(why) = bundle::check_member_size(what, size) {
                    return self.fail(bad(why));
                }
                self.current = Some((what, None));
                Ok(())
            }
            (Member::Root, _) if self.volume.is_some() => {
                let Some(volume) = self.volume.clone() else {
                    return Ok(());
                };
                let opened = volume
                    .writer
                    .make(&volume.name, size)
                    .and_then(|()| volume.writer.update(&volume.name, size));
                match opened {
                    Ok(file) => {
                        self.current = Some((
                            what,
                            Some(Writing {
                                file,
                                hasher: Sha256::new(),
                                name: name.to_string(),
                                unsynced: 0,
                            }),
                        ));
                        Ok(())
                    }
                    Err(error) => self.fail(disk("the image's volume")(error)),
                }
            }
            (Member::Root | Member::Boot, _) => {
                let path = self.work.join(name);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                match File::create(&path) {
                    Ok(file) => {
                        self.current = Some((
                            what,
                            Some(Writing {
                                file,
                                hasher: Sha256::new(),
                                name: name.to_string(),
                                unsynced: 0,
                            }),
                        ));
                        Ok(())
                    }
                    Err(error) => self.fail(disk("the work directory")(error)),
                }
            }
        }
    }

    fn data(&mut self, bytes: &[u8]) -> Result<(), String> {
        match &mut self.current {
            Some((Member::Manifest, _)) => self.manifest.extend_from_slice(bytes),
            Some((Member::Signature, _)) => self.signature.extend_from_slice(bytes),
            Some((_, Some(writing))) => {
                writing.hasher.update(bytes);
                if let Err(error) = writing.file.write_all(bytes) {
                    return self.fail(disk("the work directory")(error));
                }
                // Flushed as it comes, so no one call waits on a whole image reaching the disk:
                // a call has the host's deadline (2 s by default).
                writing.unsynced += bytes.len() as u64;
                if writing.unsynced >= SYNC_EVERY {
                    writing.unsynced = 0;
                    match Self::barrier(false, &writing.file) {
                        Ok(barrier) => self.flushes.push(barrier),
                        Err(error) => return self.fail(disk("the work directory")(error)),
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn end(&mut self) -> Result<(), String> {
        match self.current.take() {
            Some((Member::Manifest, _)) => {
                let parsed = match core::str::from_utf8(&self.manifest) {
                    Ok(text) => Manifest::parse(text).map_err(bad),
                    Err(_) => Err(bad("slot.json isn't text")),
                };
                let parsed = match parsed {
                    Ok(parsed) => parsed,
                    Err(failure) => return self.fail(failure),
                };
                // Kept before the check, so a refusal still names the slot it was for.
                let checked = self.guard.check(&parsed.id);
                self.parsed = Some(parsed);
                if let Err(failure) = checked {
                    return self.fail(failure);
                }
            }
            Some((Member::Signature, _)) => {
                // The signature is complete: decided now, before any payload member begins.
                if let Err(failure) = self.settle() {
                    return self.fail(failure);
                }
            }
            Some((_, Some(writing))) => {
                match Self::barrier(true, &writing.file) {
                    Ok(barrier) => self.flushes.push(barrier),
                    Err(error) => return self.fail(disk("the work directory")(error)),
                }
                let sha = hex(&writing.hasher.finalize());
                self.hashes.push((writing.name, sha));
            }
            _ => {}
        }
        Ok(())
    }
}

/// What `slot.sig` shows, as the shared policy takes it: the signature record names its key by
/// id (SHA-256 of the public key). A key the board holds is checked; any other is unknown. The
/// signed message is `lemnos_slot_store::signing::slot_bundle_message` over the manifest's bytes.
pub fn signature_check<'a>(keys: &'a [TrustedKey], manifest: &[u8], signature: &[u8]) -> Check<'a> {
    if signature.is_empty() {
        return Check::Unsigned;
    }
    let record = match signing::parse_record(signature) {
        Ok(record) => record,
        Err(why) => return Check::Malformed(format!("slot.sig: {why}")),
    };
    let Some(trusted) = keys
        .iter()
        .find(|t| signing::key_id(t.key.as_bytes()) == record.key_id)
    else {
        return Check::UnknownKey;
    };
    let signature = Signature::from_bytes(&record.signature);
    let message = signing::slot_bundle_message(manifest);
    if trusted.key.verify_strict(&message, &signature).is_ok() {
        Check::Verified {
            owner: &trusted.owner,
        }
    } else {
        Check::BadSignature {
            owner: &trusted.owner,
        }
    }
}

/// The board's keys and policy an install is admitted by. Given when the install starts, so the
/// signature is decided as the bundle comes, before any payload byte is written.
#[derive(Debug, Clone, Default)]
pub struct Admit {
    /// The keys the board trusts.
    pub keys: Vec<TrustedKey>,
    /// Refuse a bundle the signature table refuses (unsigned, or signed by a key the board does
    /// not trust). Off, such a bundle installs and its slot shows as untrusted or unsigned.
    pub require_signed: bool,
    /// The board file's `[board] id`: a slot built for another board is refused.
    pub board_id: String,
    /// The medium the board booted from (`medium`'s words), empty when it can't be told: a slot
    /// whose `medium` is another one is refused, and a slot of unknown medium is not.
    pub booted_medium: String,
    /// The firmware overlay paths the boot partition declares (`overlays/<name>.dtbo`), in the layout's
    /// order: a bundle's boot files must be the five roles and exactly these.
    pub firmware: Vec<String>,
    /// The boot partition's firmware config digest, `None` when it declares no lines: a bundle's
    /// `boot.config` must be the same.
    pub config: Option<String>,
}

/// What finishing needs from the board.
pub struct Finish<'a> {
    pub layout: &'a Layout,
    pub boot_args: &'a str,
    /// The board's `boot_sources` entries (`source=path`): the bundle's command line must carry
    /// `<boot_args>.src.<entry>` for each.
    pub boot_sources: &'a [String],
    pub guard: Guard,
    /// The board's boot record, which may add its own files to the slot's boot directory.
    pub record: &'a dyn crate::boot::BootRecord,
}

/// One install, from its first byte to its slot in place.
pub struct Install {
    pub owner: CallerId,
    pub job: JobId,
    pub size: u64,
    pub got: u64,
    pub pin: bool,
    pub keep_old: bool,
    tar: Tar,
    unpack: Unpack,
    /// How it ended: the slot's id, or why not.
    pub end: Option<Result<String, Failure>>,
    /// The signature policy's result for this bundle, once it was decided (at the end of slot.sig,
    /// or at the first payload member, or at `finish`): the outcome, or the refusal's text.
    pub signed: Option<Result<Outcome, String>>,
    /// Whether a failure was a refusal (what the bundle or the board says turned it away before
    /// the image was trusted) and not the work going wrong. Set when the failure is.
    pub refused: bool,
    /// The bundle's file in `.incoming`, for an install from a file: read on the driver's ticks,
    /// and deleted when the install ends.
    pub source: Option<Source>,
    /// The finish's next step, once every byte has come (see [`Install::step`]); `None` before, and
    /// after the last step.
    phase: Option<Phase>,
    /// The finish steps done, in the progress's units (see [`Install::progress`]).
    units: u64,
    /// What the finish's steps keep between ticks.
    fin: Fin,
}

/// The finish's steps, in order. Each is one call of [`Install::step`], made from the driver's tick,
/// so a step is short and the device answers other calls between two of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The bundle's checks: the tar is whole, the manifest is a slot's, the signature, board and
    /// slot are admitted, the boot files and the command line are the bundle's. Writes nothing.
    Check,
    /// slot.json, the overlays' README and the board's boot-loader files for the slot.
    Prepare,
    /// The payloads into a free bay of the boot partition.
    Payloads,
    /// The slot's boot directory into the store on the data partition: one file a step.
    BootFiles,
    /// The image and manifest into place, and the slot's metadata: the install is then in place.
    Commit,
}

impl Phase {
    fn next(self) -> Option<Phase> {
        match self {
            Phase::Check => Some(Phase::Prepare),
            Phase::Prepare => Some(Phase::Payloads),
            Phase::Payloads => Some(Phase::BootFiles),
            Phase::BootFiles => Some(Phase::Commit),
            Phase::Commit => None,
        }
    }
}

/// The finish's steps counted in the progress: one unit for each of the phases, whatever the number
/// of boot files. The progress's total is the bundle's size plus these.
pub const FINISH_UNITS: u64 = 5;

/// What the finish's steps keep from one tick to the next.
#[derive(Default)]
struct Fin {
    /// The slot's id, once the checks have named it.
    id: String,
    /// The slot's boot directory in the work directory, and its command line as the bundle has it.
    boot_dir: PathBuf,
    cmdline: String,
    /// The signature's outcome, once the checks have decided it.
    outcome: Option<Outcome>,
    /// The boot directory's copy going into the store, while its files are going in.
    put: Option<boot::Put>,
    /// Whether the payloads are in the bay and the commit has not begun: a cancel or a failure then
    /// frees the bay, since no pointer and no image names it. Set when the bay's write begins.
    bay: bool,
    /// The payload files read so far, while they are still being read (one tick's bytes at a time).
    load: Option<Load>,
    /// The bay's write, once the payloads are read: stepped a tick at a time.
    run: Option<Box<dyn crate::boot::PayloadRun>>,
}

/// The payload files read into memory, in the order they go into the bay.
#[derive(Default)]
struct Load {
    paths: Vec<String>,
    /// The index of the file being read (or the next to read).
    next: usize,
    file: Option<File>,
    /// The file being read so far, and its size.
    cur: Vec<u8>,
    size: u64,
    /// The files read whole.
    bytes: Vec<Vec<u8>>,
}

/// A bundle in `.incoming`, open.
pub struct Source {
    pub file: File,
    pub path: PathBuf,
}

/// The directory a sender puts a bundle in for `install_file`, under the images directory.
pub const INCOMING: &str = ".incoming";
/// How much of a file one tick reads: small enough that a tick stays far inside a call's
/// deadline.
pub const READ_EVERY_TICK: usize = 1 << 20;

/// Whether `name` may name a file in `.incoming`: one path component of `[A-Za-z0-9._-]`, not
/// `.` or `..`.
pub fn valid_incoming(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.len() <= 255
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Opens `name` in `incoming` for an install: never through a link, and only a regular file.
pub fn open_incoming(incoming: &Path, name: &str) -> Result<(Source, u64), (Status, String)> {
    use std::os::unix::fs::OpenOptionsExt;
    if !valid_incoming(name) {
        return Err((Status::INVALID_ARGUMENT, words::BAD_NAME.to_string()));
    }
    let path = incoming.join(name);
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|error| match error.raw_os_error() {
            Some(libc::ENOENT) => (Status::NOT_FOUND, words::NOT_IN_INCOMING.to_string()),
            // ELOOP: the name is a link.
            Some(libc::ELOOP) => (Status::INVALID_ARGUMENT, words::A_LINK.to_string()),
            _ => (
                Status::from_io_error(&error),
                format!("the bundle can't be opened: {error}"),
            ),
        })?;
    let meta = file.metadata().map_err(|error| {
        (
            Status::from_io_error(&error),
            format!("the bundle can't be read: {error}"),
        )
    })?;
    if !meta.is_file() {
        return Err((Status::INVALID_ARGUMENT, words::NOT_A_FILE.to_string()));
    }
    Ok((Source { file, path }, meta.len()))
}

/// Clears `.incoming` and makes sure it is there: mode 0770, group `lemnos` when the system
/// has one, so a sender in that group can write into it.
pub fn reset_incoming(layout: &Layout) -> io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let dir = layout.images().join(INCOMING);
    fs::create_dir_all(&dir)?;
    for entry in fs::read_dir(&dir)?.flatten() {
        let path = entry.path();
        let _ = if entry.file_type().is_ok_and(|t| t.is_dir()) {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o770))?;
    if let Some(gid) = group_id(
        &fs::read_to_string("/etc/group").unwrap_or_default(),
        "lemnos",
    ) {
        // Left as it is when the service may not change it: the sender then needs another way in.
        let _ = std::os::unix::fs::chown(&dir, None, Some(gid));
    }
    Ok(dir)
}

/// A group's id from `/etc/group`'s text.
pub fn group_id(text: &str, name: &str) -> Option<u32> {
    text.lines().find_map(|line| {
        let mut fields = line.split(':');
        (fields.next()? == name).then_some(())?;
        fields.nth(1)?.parse().ok()
    })
}

impl Install {
    /// Starts one into a new work directory in the images directory.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        layout: &Layout,
        owner: CallerId,
        job: JobId,
        size: u64,
        pin: bool,
        keep_old: bool,
        guard: Guard,
        admit: Admit,
        writer: Option<Arc<dyn UbiWrite>>,
    ) -> Result<Self, Failure> {
        let work = layout.images().join(format!("{WORK_PREFIX}{}", job.0));
        let volume = writer.map(|writer| WorkVolume {
            writer,
            name: ubi::work_volume(job.0),
        });
        if let (Some(volume), Some(device)) = (&volume, &layout.ubi)
            && device.volume(&volume.name).is_some()
        {
            volume
                .writer
                .remove(&volume.name)
                .map_err(disk("the work volume"))?;
        }
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).map_err(disk("the work directory"))?;
        Ok(Self {
            owner,
            job,
            size,
            got: 0,
            pin,
            keep_old,
            tar: Tar::default(),
            unpack: Unpack {
                work,
                volume,
                guard,
                admit,
                manifest: Vec::new(),
                signature: Vec::new(),
                parsed: None,
                entries: Vec::new(),
                decided: None,
                signed: None,
                current: None,
                hashes: Vec::new(),
                flushes: Vec::new(),
                failure: None,
            },
            end: None,
            signed: None,
            refused: false,
            source: None,
            phase: None,
            units: 0,
            fin: Fin::default(),
        })
    }

    /// Takes the next bytes. An error ends the install; the caller sets `end`.
    pub fn take(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Failure> {
        if offset != self.got {
            return Err(bad(format!(
                "bytes at {offset}, where the install is at {}",
                self.got
            )));
        }
        let after = self.got.saturating_add(bytes.len() as u64);
        if after > self.size {
            return Err(bad(format!("more than the {} bytes announced", self.size)));
        }
        let fed = self.tar.feed(bytes, &mut self.unpack);
        self.signed = self.unpack.signed.clone();
        if let Err(text) = fed {
            let failure = self.unpack.failure.take().unwrap_or_else(|| bad(text));
            // What the bundle's words or the board's rules refuse is a refusal; the disk failing
            // is the work going wrong.
            self.refused = failure.0 != Status::IO;
            return Err(failure);
        }
        self.got = after;
        Ok(())
    }

    /// Whether every byte has come.
    pub fn complete(&self) -> bool {
        self.got == self.size
    }

    /// The bytes have all come: the finish's steps run from here, one a call of [`Install::step`].
    pub fn begin_finish(&mut self) {
        if self.phase.is_none() && self.complete() && self.end.is_none() {
            self.phase = Some(Phase::Check);
        }
    }

    /// Whether the finish's steps are still to run.
    pub fn finishing(&self) -> bool {
        self.phase.is_some()
    }

    /// The progress the install stands at: `(done, total)`. The bytes taken, then one unit for
    /// each finish step done, so the figure never goes back when the last byte comes.
    pub fn progress(&self) -> (u64, u64) {
        (self.got + self.units, self.size + FINISH_UNITS)
    }

    /// The slot whose payloads are in its bay while the commit has not begun: a cancel or a failure
    /// frees that bay, since no pointer and no image name it.
    pub fn bay_slot(&self) -> Option<&str> {
        self.fin.bay.then_some(self.fin.id.as_str())
    }

    /// Runs the next finish step. `Some(id)` once the slot is in place; `None` while steps remain.
    /// A failure ends the finish: the caller then ends the install.
    pub fn step(
        &mut self,
        board: &Finish<'_>,
        budget: &mut crate::budget::Budget<'_>,
    ) -> Result<Option<String>, Failure> {
        let Some(phase) = self.phase else {
            return Ok(None);
        };
        let mut refused = false;
        let done = match phase {
            Phase::Check => self.check(board, &mut refused),
            Phase::Prepare => self.prepare(board, &mut refused),
            Phase::Payloads => self.payloads(board, budget, &mut refused),
            Phase::BootFiles => self.boot_files(board, budget, &mut refused),
            Phase::Commit => self.commit(board, &mut refused),
        };
        self.refused = refused;
        match done {
            Err(failure) => {
                self.phase = None;
                Err(failure)
            }
            Ok(false) => Ok(None),
            Ok(true) => {
                self.units += 1;
                self.phase = phase.next();
                Ok(self.phase.is_none().then(|| self.fin.id.clone()))
            }
        }
    }

    /// The bundle's checks: the tar is whole; the manifest is a slot's; the signature, the board and
    /// the slot are admitted; the boot files and the command line are the bundle's. Writes nothing.
    fn check(&mut self, board: &Finish<'_>, refused: &mut bool) -> Result<bool, Failure> {
        if !self.tar.finished() {
            return Err(bad("the bundle ends in the middle"));
        }
        // The bundle's files must be on the medium before anything names them: wait for the stream's
        // barriers (the next tick polls again).
        if !self.unpack.flushed()? {
            return Ok(false);
        }
        *refused = true;
        if self.unpack.manifest.is_empty() {
            return Err(bad("not a slot bundle: no slot.json"));
        }
        // The signature and the board were decided as the bundle came (before any payload byte);
        // a bundle with no payload is decided here, as unsigned or by its slot.sig.
        let decided = self.unpack.settle();
        self.signed = self.unpack.signed.clone();
        let outcome = decided?;
        let unpack = &self.unpack;
        let manifest = unpack
            .parsed
            .clone()
            .ok_or_else(|| bad("slot.json isn't a slot manifest"))?;
        let id = manifest.id.clone();
        board.guard.check(&id)?;
        *refused = false;
        let root_hash = manifest.verity.root_hash.as_str();
        let root = unpack.work.join("rootfs.erofs");
        let size = match (&unpack.volume, &board.layout.ubi) {
            (Some(volume), Some(device)) => device.volume(&volume.name).map(|v| v.bytes),
            _ => fs::metadata(&root).ok().map(|m| m.len()),
        }
        .filter(|_| unpack.hashes.iter().any(|(n, _)| n == "rootfs.erofs"));
        bundle::check_root(&manifest, size, &unpack.hashes).map_err(bad)?;
        if let Err(why) = bundle::check_boot(&manifest, &unpack.hashes) {
            // A listed boot file missing or different is the work going wrong; a file the manifest
            // does not list, with every listed one in place, is a refusal.
            *refused = manifest.boot_files.iter().all(|(name, want)| {
                unpack
                    .hashes
                    .iter()
                    .any(|(n, h)| n.strip_prefix("boot/") == Some(name.as_str()) && h == want)
            });
            return Err(bad(why));
        }
        *refused = true;
        let boot_dir = unpack.work.join("boot");
        let args = board.boot_args;
        if args.is_empty() || !args.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err((
                Status::NO_DEVICE,
                "the board file names no boot_args for the slot's initramfs".into(),
            ));
        }
        // The command line that boots is the bundle's, byte for byte: its hash is in the signed
        // manifest's boot files, so nothing is written into it here. The bundle names its own slot
        // and root hash; a bundle whose line does not is refused.
        let line = fs::read_to_string(boot_dir.join("cmdline.txt"))
            .map_err(|_| bad("the bundle has no boot/cmdline.txt"))?;
        let cmdline = bundle::cmdline_line(&line).to_string();
        bundle::check_cmdline(args, &id, root_hash, board.boot_sources, &cmdline).map_err(bad)?;
        *refused = false;
        self.fin = Fin {
            id,
            boot_dir,
            cmdline,
            outcome: Some(outcome),
            ..Fin::default()
        };
        Ok(true)
    }

    /// The boot side for the slot, kept on the data partition's side: its `slot.json`, the overlays
    /// README, and the board's boot-loader files (`prepare`). A slot whose directory is already on the
    /// boot partition (installed again) gets its new files there now.
    fn prepare(&mut self, board: &Finish<'_>, refused: &mut bool) -> Result<bool, Failure> {
        let io = disk("the slot's boot files");
        *refused = false;
        let boot_dir = self.fin.boot_dir.clone();
        fs::write(boot_dir.join("slot.json"), &self.unpack.manifest).map_err(&io)?;
        let readme = boot_dir.join("overlays/README");
        if boot_dir.join("overlays").is_dir() && !readme.exists() {
            fs::write(
                readme,
                "Overlays for this slot. The firmware reads them from os_prefix only when this file exists.\n",
            )
            .map_err(&io)?;
        }
        *refused = true;
        board
            .record
            .prepare(&boot_dir, &self.fin.id, &self.fin.cmdline)
            .map_err(|error| bad(format!("the boot loader's files for this slot: {error}")))?;
        Ok(true)
    }

    /// The payloads go into a free bay of the boot partition: the five roles, then the firmware
    /// overlays the boot partition declares (the admission made the bundle's set exactly these). The
    /// files are read and the bay's write is stepped a tick's [`crate::budget::Budget`] at a time, so
    /// no tick spends more than its share of the call deadline or [`READ_EVERY_TICK`] bytes. Refusals come first (a payload missing or larger than its bay is refused by name
    /// before any write); no pointer moves.
    fn payloads(
        &mut self,
        board: &Finish<'_>,
        budget: &mut crate::budget::Budget<'_>,
        refused: &mut bool,
    ) -> Result<bool, Failure> {
        *refused = true;
        if self.fin.run.is_none() {
            if !self.load_some(budget)? {
                return Ok(false);
            }
            let bytes = self
                .fin
                .load
                .take()
                .map(|load| load.bytes)
                .unwrap_or_default();
            let run = board
                .record
                .payload_begin(&self.fin.id, bytes)
                .map_err(|error| bad(format!("slot {}'s boot files: {error}", self.fin.id)))?;
            self.fin.run = Some(run);
            self.fin.bay = true;
        }
        let Some(run) = self.fin.run.as_mut() else {
            return Ok(false);
        };
        let done = run
            .step(budget)
            .map_err(|error| bad(format!("slot {}'s boot files: {error}", self.fin.id)))?;
        if done {
            self.fin.run = None;
            *refused = false;
        }
        Ok(done)
    }

    /// Reads the payload files a piece at a time until the tick's budget is spent. `Ok(true)` once
    /// all are read.
    fn load_some(&mut self, budget: &mut crate::budget::Budget<'_>) -> Result<bool, Failure> {
        let io = disk("the slot's boot files");
        let boot_dir = self.fin.boot_dir.clone();
        let load = self.fin.load.get_or_insert_with(|| {
            let mut paths: Vec<String> = lemnos_slot_store::bays::stored_files()
                .iter()
                .map(|p| p.to_string())
                .collect();
            paths.extend(self.unpack.admit.firmware.iter().cloned());
            Load {
                paths,
                ..Load::default()
            }
        });
        loop {
            let Some(path) = load.paths.get(load.next).cloned() else {
                return Ok(true);
            };
            if load.file.is_none() {
                let file = File::open(boot_dir.join(&path))
                    .map_err(|_| bad(format!("the bundle has no boot/{path}")))?;
                load.size = file.metadata().map_err(&io)?.len();
                load.cur = Vec::with_capacity(usize::try_from(load.size).unwrap_or(0));
                load.file = Some(file);
            }
            let want = (load.size - load.cur.len() as u64).min(budget.piece() as u64) as usize;
            if want > 0 {
                let from = load.cur.len();
                load.cur.resize(from + want, 0);
                let file = load.file.as_mut().expect("opened above");
                std::io::Read::read_exact(file, &mut load.cur[from..]).map_err(&io)?;
                budget.spend(want);
            }
            if (load.cur.len() as u64) == load.size {
                load.bytes.push(std::mem::take(&mut load.cur));
                load.file = None;
                load.next += 1;
            }
            if budget.spent() {
                return Ok(false);
            }
        }
    }

    /// The slot's boot directory into the boot partition's store on the data side, a tick's budget at
    /// a time ([`boot::Put`]).
    fn boot_files(
        &mut self,
        board: &Finish<'_>,
        budget: &mut crate::budget::Budget<'_>,
        refused: &mut bool,
    ) -> Result<bool, Failure> {
        let io = disk("the slot's boot files");
        *refused = false;
        if self.fin.put.is_none() {
            let put =
                boot::Put::begin(board.layout, &self.fin.id, &self.fin.boot_dir).map_err(&io)?;
            self.fin.put = Some(put);
        }
        let Some(put) = self.fin.put.as_mut() else {
            return Ok(true);
        };
        let done = put.step(budget).map_err(&io)?;
        if done {
            self.fin.put = None;
        }
        Ok(done)
    }

    /// The image and the manifest go into place by rename, then the slot's metadata. From the first
    /// rename the install is past any undo: a cancel or a failure no longer frees the bay.
    fn commit(&mut self, board: &Finish<'_>, refused: &mut bool) -> Result<bool, Failure> {
        let io = disk("the slot's boot files");
        *refused = false;
        let layout = board.layout;
        let id = self.fin.id.clone();
        let images = layout.images();
        let manifest_tmp = images.join(format!(".{id}.json.tmp"));
        fs::write(&manifest_tmp, &self.unpack.manifest).map_err(&io)?;
        File::open(&manifest_tmp)
            .and_then(|f| f.sync_all())
            .map_err(&io)?;
        // The bundle's slot.sig, kept unchanged beside the manifest it signs: with `<id>.json` it rebuilds
        // the bundle's signed part. Written before the commit point (the manifest's rename), so a power cut
        // never leaves a manifest with half a signature.
        let signature_tmp = images.join(format!(".{id}.sig.tmp"));
        if !self.unpack.signature.is_empty() {
            fs::write(&signature_tmp, &self.unpack.signature).map_err(&io)?;
            File::open(&signature_tmp)
                .and_then(|f| f.sync_all())
                .map_err(&io)?;
        }
        self.fin.bay = false;
        let unpack = &self.unpack;
        match &unpack.volume {
            Some(volume) => volume
                .writer
                .rename(&volume.name, &ubi::image_volume(&id))
                .map_err(disk("the image's volume"))?,
            None => fs::rename(unpack.work.join("rootfs.erofs"), layout.image(&id)).map_err(&io)?,
        }
        crate::store::sync_dir(&images).map_err(&io)?;
        if self.unpack.signature.is_empty() {
            // An unsigned bundle keeps none, and an earlier install's signature is not this manifest's.
            match fs::remove_file(layout.signature(&id)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(io(error)),
            }
        } else {
            fs::rename(&signature_tmp, layout.signature(&id)).map_err(&io)?;
        }
        fs::rename(&manifest_tmp, layout.manifest(&id)).map_err(&io)?;
        crate::store::sync_dir(&images).map_err(&io)?;
        let _ = fs::remove_dir_all(&unpack.work);
        let mut meta = Meta::default();
        meta.set("PINNED", if self.pin { "yes" } else { "no" });
        meta.set("GOOD", "untried");
        // `SIGNATURE` is the outcome word; `SIGNED` keeps the label older boards and readers use.
        let Some(outcome) = self.fin.outcome.clone() else {
            return Err(bad("the bundle's signature was not decided"));
        };
        meta.set("SIGNATURE", outcome.word());
        meta.set("SIGNED", outcome.legacy_word());
        meta.set("SIGNER", outcome.signer());
        meta.set("INSTALLED", &crate::store::now_utc());
        let seq = crate::store::next_seq(layout).map_err(&io)?;
        meta.set("SEQ", &seq.to_string());
        meta.save(layout, &id).map_err(&io)?;
        Ok(true)
    }

    /// The slot the bundle's manifest names, once it has come and parsed.
    pub fn slot_id(&self) -> Option<&str> {
        self.unpack.parsed.as_ref().map(|m| m.id.as_str())
    }

    /// Removes what came so far.
    pub fn discard(&mut self) {
        self.phase = None;
        let _ = fs::remove_dir_all(&self.unpack.work);
        if let Some(volume) = &self.unpack.volume {
            let _ = volume.writer.remove(&volume.name);
        }
        self.drop_source();
    }

    /// Deletes the bundle's file in `.incoming`, if the install came from one.
    pub fn drop_source(&mut self) {
        if let Some(source) = self.source.take() {
            let _ = fs::remove_file(&source.path);
        }
    }

    /// Reads the next piece of the source file into `buf`: how much.
    pub fn pull(&mut self, buf: &mut [u8]) -> Result<usize, Failure> {
        use std::io::Read;
        let Some(source) = self.source.as_mut() else {
            return Ok(0);
        };
        let want = usize::try_from(self.size - self.got)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        let mut read = 0;
        while read < want {
            match source.file.read(&mut buf[read..want]) {
                Ok(0) => break,
                Ok(n) => read += n,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(disk("the bundle's file")(error)),
            }
        }
        if read == 0 && want > 0 {
            return Err(bad(format!(
                "the bundle's file ended at {} of its {} bytes",
                self.got, self.size
            )));
        }
        Ok(read)
    }
}

/// Work an install left when the service stopped in the middle: removed. Whether there was any.
pub fn clear_leftovers(layout: &Layout, writer: Option<&dyn UbiWrite>) -> bool {
    let mut found = false;
    if let (Some(device), Some(writer)) = (&layout.ubi, writer) {
        for volume in device.volumes() {
            if volume.name.starts_with(ubi::WORK_PREFIX) {
                found = true;
                let _ = writer.remove(&volume.name);
            }
        }
    }
    for entry in fs::read_dir(layout.images())
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(WORK_PREFIX) || (name.starts_with('.') && name.ends_with(".tmp")) {
            found = true;
            let _ = if entry.path().is_dir() {
                fs::remove_dir_all(entry.path())
            } else {
                fs::remove_file(entry.path())
            };
        }
    }
    found
}
